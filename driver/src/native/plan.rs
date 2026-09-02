//! Issuing the one-off statements that turn an empty database into a loaded
//! one.
//!
//! The statements come from `clickdoom_native`, which decides what they are
//! and what order they go in. This issues them, times each group and stops
//! at the first failure.

use std::time::{Duration, Instant}; // purity-ok: reporting how long a load took, read by no statement

use clickdoom_native::sql::{self, Statement};
use clickdoom_native::{load, wad::Wad};

use crate::client::{self, Db};
use crate::native::melt;

/// The phases that fill a database from a WAD, in the only order their own
/// documentation allows: the level decode reads the lumps the base phase
/// inserts, and the renderer's tables and the simulation's first state row
/// read the decoded level.
///
/// The database has to be empty of these tables already. Emptying it is the
/// caller's, because a command and a test start from different places.
pub fn level_phases(
    database: &str,
    wad: &Wad<'_>,
    map: &str,
    demo: &str,
    sky: &str,
) -> Result<Vec<Phase>, melt::UnknownDemo> {
    Ok(vec![
        Phase::new("base", load::plan(database, wad)),
        Phase::new("level", sql::level_statements(database, map, demo)),
        Phase::new("render", sql::render_statements(database, sky)),
        Phase::new("sim", sql::sim::load_statements(database)),
        Phase::new("melt", melt::load_statements(database, demo)?),
    ])
}

/// A statement the server refused, with enough of the text to find it.
#[derive(Debug, thiserror::Error)]
#[error("{phase}: {head}: {source}")]
pub struct PlanError {
    /// The phase the statement belonged to.
    pub phase: String,
    /// The statement's first line, cut short.
    pub head: String,
    #[source]
    pub source: client::Error,
}

/// A named group of statements, issued in order.
pub struct Phase {
    pub name: &'static str,
    pub statements: Vec<Statement>,
}

impl Phase {
    pub fn new(name: &'static str, statements: Vec<Statement>) -> Phase {
        Phase { name, statements }
    }
}

/// What one phase cost.
pub struct PhaseReport {
    pub name: &'static str,
    pub statements: usize,
    /// Bytes of statement body streamed, over every statement that had one.
    pub body_bytes: usize,
    pub elapsed: Duration,
}

/// Issues every phase in order and reports what each cost.
///
/// The first statement the server refuses stops the run, so a later phase
/// never runs against a half-built database.
pub async fn run(db: &Db, phases: &[Phase]) -> Result<Vec<PhaseReport>, PlanError> {
    let mut reports = Vec::with_capacity(phases.len());
    for phase in phases {
        let started = Instant::now(); // purity-ok: timing the phase, see the import
        let mut body_bytes = 0;
        for statement in &phase.statements {
            issue(db, statement).await.map_err(|source| PlanError {
                phase: phase.name.to_owned(),
                head: head(&statement.sql),
                source,
            })?;
            body_bytes += statement.body.len();
        }
        reports.push(PhaseReport {
            name: phase.name,
            statements: phase.statements.len(),
            body_bytes,
            elapsed: started.elapsed(),
        });
    }
    Ok(reports)
}

/// One statement. A body goes as the request body; the HTTP interface takes
/// one statement per request either way.
async fn issue(db: &Db, statement: &Statement) -> Result<(), client::Error> {
    if statement.body.is_empty() {
        return db.run(&statement.sql).await;
    }
    db.run_with_body(
        &statement.sql,
        bytes::Bytes::copy_from_slice(&statement.body),
    )
    .await
}

/// How much of a statement an error quotes.
const HEAD_CHARS: usize = 120;

fn head(sql: &str) -> String {
    let line = sql.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(HEAD_CHARS) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_quotes_the_statements_first_line() {
        assert_eq!(head("CREATE TABLE t\n(x UInt32)"), "CREATE TABLE t");
        let long = format!("SELECT '{}'", "x".repeat(200));
        assert!(head(&long).ends_with("..."));
        assert_eq!(head(&long).chars().count(), HEAD_CHARS + 3);
    }
}
