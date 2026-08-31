//! What every check in the suite shares: the connection, the private
//! database, the error type, and the roster that decides whether the run as
//! a whole passed.

use std::path::PathBuf;

use clickdoom_driver::client::{self, ConnArgs, Db};
use clickdoom_driver::sql::split_statements;
use clickhouse::Row;
use serde::Serialize;

/// `sqlcpu/schema.sql` names its own database `clickdoom`. Every reference
/// is rewritten to the private per-run database, the same substitution
/// `preflight` makes for its throwaway reference database.
const SCHEMA_SQL: &str = include_str!("../../../sqlcpu/schema.sql");

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// A statement the server rejected. `what` says which step issued it,
    /// `first_line` identifies the statement itself: a generated fold is
    /// tens of kilobytes and its first line names the tables it reads.
    #[error("{what}: {source}\n  statement began: {first_line}")]
    Query {
        what: String,
        first_line: String,
        #[source]
        source: client::Error,
    },
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A check ran and disagreed with what it was checking. The message is
    /// the whole report.
    #[error("{0}")]
    Mismatch(String),
}

/// One row of `ram`: the fixture words a check loads before decoding.
#[derive(Row, Serialize)]
pub struct WordRow {
    pub word_addr: u32,
    pub value: u32,
    pub version: u64,
}

fn first_line(sql: &str) -> String {
    sql.lines().next().unwrap_or_default().trim().to_owned()
}

/// Runs a statement that returns no rows, naming the step in any error.
pub async fn run(db: &Db, what: &str, sql: &str) -> Result<(), CheckError> {
    db.run(sql).await.map_err(|source| CheckError::Query {
        what: what.to_owned(),
        first_line: first_line(sql),
        source,
    })
}

/// Runs a statement and fetches a single row, naming the step in any error.
pub async fn fetch_one<T>(db: &Db, what: &str, sql: &str) -> Result<T, CheckError>
where
    T: clickhouse::RowOwned + clickhouse::RowRead,
{
    db.fetch_one::<T>(sql)
        .await
        .map_err(|source| CheckError::Query {
            what: what.to_owned(),
            first_line: first_line(sql),
            source,
        })
}

/// Runs a statement and fetches every row, naming the step in any error.
pub async fn fetch_all<T>(db: &Db, what: &str, sql: &str) -> Result<Vec<T>, CheckError>
where
    T: clickhouse::RowOwned + clickhouse::RowRead,
{
    db.fetch_all::<T>(sql)
        .await
        .map_err(|source| CheckError::Query {
            what: what.to_owned(),
            first_line: first_line(sql),
            source,
        })
}

/// Inserts rows into `table`, naming the step in any error.
pub async fn insert_all<T>(
    db: &Db,
    what: &str,
    table: &str,
    rows: impl Iterator<Item = T>,
) -> Result<(), CheckError>
where
    T: clickhouse::RowOwned + clickhouse::RowWrite,
{
    db.insert_all(table, rows)
        .await
        .map_err(|source| CheckError::Query {
            what: what.to_owned(),
            first_line: format!("INSERT INTO {table}"),
            source,
        })
}

/// Where the server is. `CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT` place it,
/// `CLICKHOUSE_PASSWORD` authenticates, all read by `ConnArgs` itself.
pub fn conn_args() -> ConnArgs {
    ConnArgs {
        host: std::env::var("CLICKHOUSE_HOST").unwrap_or_else(|_| "localhost".to_owned()),
        port: std::env::var("CLICKHOUSE_HTTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8123),
        user: "default".to_owned(),
        database: "default".to_owned(),
        password: None,
    }
}

pub fn db_at(conn: &ConnArgs, database: &str) -> Db {
    let mut conn = conn.clone();
    conn.database = database.to_owned();
    conn.connect()
}

/// Creates `database` from `sqlcpu/schema.sql`, dropping any leftover of
/// the same name first.
pub async fn create_database(admin: &Db, database: &str) -> Result<(), CheckError> {
    run(
        admin,
        "dropping a leftover suite database",
        &format!("DROP DATABASE IF EXISTS {database}"),
    )
    .await?;
    let schema = SCHEMA_SQL
        .replace("clickdoom.", &format!("{database}."))
        .replace(
            "CREATE DATABASE IF NOT EXISTS clickdoom;",
            &format!("CREATE DATABASE IF NOT EXISTS {database};"),
        );
    for statement in split_statements(&schema) {
        run(admin, "applying sqlcpu/schema.sql", statement).await?;
    }
    Ok(())
}

/// The checks a run must account for, and what each of them reported.
///
/// A check that reports nothing fails the run. A summary line cannot tell a
/// skipped check from a passing one, so the roster is what stands between
/// "the check is gone" and a green run.
pub struct Report {
    required: &'static [&'static str],
    outcomes: Vec<(String, Result<String, CheckError>)>,
}

impl Report {
    pub fn new(required: &'static [&'static str]) -> Self {
        Report {
            required,
            outcomes: Vec::new(),
        }
    }

    pub fn record(&mut self, name: &str, outcome: Result<String, CheckError>) {
        self.outcomes.push((name.to_owned(), outcome));
    }

    /// Prints every outcome, then reports what is wrong with the run as a
    /// whole: a check that failed, a required check that reported nothing,
    /// a check reported twice, or a name that is not on the roster.
    pub fn finish(self) -> Result<(), String> {
        let mut failures = Vec::new();
        for (name, outcome) in &self.outcomes {
            match outcome {
                Ok(summary) => println!("{name}: {summary}"),
                Err(error) => {
                    println!("{name}: FAILED");
                    failures.push(format!("{name}: {error}"));
                }
            }
            if !self.required.contains(&name.as_str()) {
                failures.push(format!(
                    "{name} is not on the roster: add it to REQUIRED_CHECKS or stop reporting it"
                ));
            }
        }
        for required in self.required {
            let reported = self
                .outcomes
                .iter()
                .filter(|(name, _)| name == required)
                .count();
            if reported != 1 {
                failures.push(format!(
                    "{required} reported {reported} outcomes, expected exactly 1: a check the \
                     sequencer no longer runs is indistinguishable from one that passed"
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n"))
        }
    }
}
