//! Loading the reference emulator's probe rows.
//!
//! The table and the insert that fills it are
//! [`clickdoom_native::sql::probe`], which takes its column types from the
//! `native_state` declaration so the two cannot drift apart. What is here
//! is reading the file, issuing them, and the copy into `native_state` that
//! a run rendering from probed states needs.

use std::path::{Path, PathBuf};

use clickdoom_native::sql::probe as shape;

use crate::client::{self, Db};
use crate::native::plan;

/// The table the probe rows land in.
pub const STAGING_TABLE: &str = "probe_state";

/// The table the rows are copied into, keyed by tic.
pub const STATE_TABLE: &str = "native_state";

/// Columns the probe writes ahead of the contract's field list, which the
/// copy into `native_state` does not carry over: the frame index and the
/// frame hash belong to the frame, and the gametic becomes the tic.
const LEADING: usize = 3;

/// Anything that stops a probe file from loading.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "{path} is not a probe file this tree can read: {source}. \
         Regenerate it against this tree with `make gen-probe-trace`"
    )]
    Shape {
        path: PathBuf,
        #[source]
        source: shape::Error,
    },
    #[error(transparent)]
    Plan(#[from] plan::PlanError),
    #[error(transparent)]
    Db(#[from] client::Error),
}

/// What one load put in the database.
pub struct Loaded {
    /// Rows the probe file carried.
    pub rows: u64,
    /// Distinct tics they cover, which is what `native_state` gains.
    pub tics: u64,
    /// The lowest and highest frame index in the file.
    pub frames: (u32, u32),
}

/// Loads `path` into `{database}.probe_state` and copies its rows into
/// `native_state`, keyed by the gametic each was taken at.
pub async fn load(db: &Db, database: &str, path: &Path) -> Result<Loaded, Error> {
    let loaded = stage(db, database, path).await?;
    into_state(db, database).await?;
    Ok(loaded)
}

/// Loads `path` into `{database}.probe_state` and leaves `native_state`
/// alone.
///
/// A differential run wants exactly this: the two sides have to be two
/// tables, and copying the probe over the simulation's own rows would
/// compare a run against itself.
///
/// Re-running replaces what a previous load put there, because the table is
/// dropped first rather than added to.
pub async fn stage(db: &Db, database: &str, path: &Path) -> Result<Loaded, Error> {
    let tsv = std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    let insert = shape::insert(database, &tsv).map_err(|source| Error::Shape {
        path: path.to_owned(),
        source,
    })?;
    let statements = vec![
        clickdoom_native::sql::Statement::sql(format!(
            "DROP TABLE IF EXISTS {database}.{STAGING_TABLE}"
        )),
        shape::schema_statement(database),
        insert,
    ];
    plan::run(db, &[plan::Phase::new("probe", statements)]).await?;

    let (rows, tics, low, high) = db
        .fetch_one::<(u64, u64, u32, u32)>(&format!(
            "SELECT count(), uniqExact(gametic), min(frame_index), max(frame_index) \
             FROM {database}.{STAGING_TABLE}"
        ))
        .await?;
    Ok(Loaded {
        rows,
        tics,
        frames: (low, high),
    })
}

/// Copies the staged rows into `native_state`, keyed by the gametic each
/// was taken at, so a run can render from them.
///
/// The two tables declare the same fields at the same types, so this names
/// the columns and copies them. `native_state` keys on the tic, and the
/// frames the melt drew all share one, so a re-run replaces rather than
/// adds.
pub async fn into_state(db: &Db, database: &str) -> Result<(), Error> {
    db.run(&copy_into_state(database)).await?;
    Ok(())
}

/// The copy, naming every column it writes. The columns `native_state`
/// declares that the probe does not carry stay at their default.
fn copy_into_state(database: &str) -> String {
    let fields = shape::names()[LEADING..].join(", ");
    format!(
        "INSERT INTO {database}.{STATE_TABLE} (tic, {fields}) \
         SELECT gametic, {fields} FROM {database}.{STAGING_TABLE}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three the probe writes in front are the frame's, not the tic's.
    #[test]
    fn the_copy_takes_the_tic_from_the_gametic_and_leaves_the_frame_behind() {
        let sql = copy_into_state("nat");
        assert!(
            sql.starts_with("INSERT INTO nat.native_state (tic, leveltime,"),
            "{sql}"
        );
        assert!(sql.contains("SELECT gametic, leveltime,"), "{sql}");
        for frame_column in ["frame_index", "fb_hash"] {
            assert!(!sql.contains(frame_column), "{frame_column} is in {sql}");
        }
    }

    /// Every field the probe carries is written, so a column added to the
    /// contract cannot be left behind silently.
    #[test]
    fn every_field_the_probe_carries_is_named() {
        let sql = copy_into_state("nat");
        for field in &shape::names()[LEADING..] {
            assert!(sql.contains(field), "{field} is not copied");
        }
    }
}
