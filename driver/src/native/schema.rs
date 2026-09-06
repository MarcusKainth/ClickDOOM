//! Whether a database still matches the schema this binary carries.
//!
//! `CREATE TABLE IF NOT EXISTS` leaves an existing table's columns alone,
//! so a load that only empties tables (no `--fresh`) never notices a
//! column that changed type or one a newer schema added. [`check_columns`]
//! is `native load`'s own guard against that: it reads back every declared
//! table's actual columns and refuses before touching anything, naming the
//! first table and column that differ.
//!
//! [`check_hash`] is the other half, for `native diff`, `native demo` and
//! `native play`: a load writes this binary's own `schema_hash` the moment
//! it finishes, and a run refuses a database whose row does not match,
//! rather than reading state rows a schema check alone could not rule out
//! (a column reordered, one added to a table nothing else touches, or a
//! constant table `native load`'s own column check does not reach).

use clickhouse::Row;
use serde::Deserialize;

use crate::client::{Db, Error};

/// A declared column this binary expects that the database does not carry
/// the way `native/schema.sql` says.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Mismatch {
    #[error(
        "{database}.{table} has no column {column}; schema.sql declares it as {want}. \
         Pass --fresh to drop and reload"
    )]
    Missing {
        database: String,
        table: String,
        column: String,
        want: String,
    },
    #[error(
        "{database}.{table}.{column} is {got}, schema.sql declares {want}. \
         Pass --fresh to drop and reload"
    )]
    Changed {
        database: String,
        table: String,
        column: String,
        want: String,
        got: String,
    },
    #[error(
        "{database}'s schema hash is {got}, this binary's own is {want}. \
         Load it fresh with this binary before reading it"
    )]
    Stale {
        database: String,
        want: u64,
        got: String,
    },
}

#[derive(Row, Deserialize)]
struct ColumnRow {
    table: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
}

/// Every declared column this binary's schema names, checked against what
/// `database` actually carries. The first one that differs, if any; `Ok`
/// carries the mismatch rather than raising it, since a caller decides on
/// its own whether that refuses the run.
///
/// A table `database` does not carry at all is not a mismatch: `CREATE
/// TABLE IF NOT EXISTS` makes it. A table that exists but is missing a
/// column, or carries it at a different type, is: reloading over it would
/// either fail outright or, worse, let ClickHouse narrow a wider value to
/// fit silently.
pub async fn check_columns(db: &Db, database: &str) -> Result<Option<Mismatch>, Error> {
    let existing: Vec<ColumnRow> = db
        .fetch_all(&format!(
            "SELECT table, name, type FROM system.columns WHERE database = '{database}'"
        ))
        .await?;
    for (table, column, want) in clickdoom_native::sql::schema_columns() {
        if !existing.iter().any(|row| row.table == table) {
            continue;
        }
        let mismatch = match existing
            .iter()
            .find(|row| row.table == table && row.name == column)
        {
            None => Some(Mismatch::Missing {
                database: database.to_owned(),
                table: table.to_owned(),
                column: column.to_owned(),
                want: want.to_owned(),
            }),
            Some(found) if clickdoom_native::sql::collapse_type(&found.kind) != want => {
                Some(Mismatch::Changed {
                    database: database.to_owned(),
                    table: table.to_owned(),
                    column: column.to_owned(),
                    want: want.to_owned(),
                    got: found.kind.clone(),
                })
            }
            Some(_) => None,
        };
        if mismatch.is_some() {
            return Ok(mismatch);
        }
    }
    Ok(None)
}

/// `database`'s own `schema_hash`, checked against this binary's.
///
/// A database an older binary loaded, before `schema_hash` existed, reads
/// back as a mismatch too: nothing here can tell that database's schema
/// from a stale one, so it is treated as one.
pub async fn check_hash(db: &Db, database: &str) -> Option<Mismatch> {
    let want = clickdoom_native::sql::schema_hash();
    match db
        .fetch_one::<u64>(&format!("SELECT hash FROM {database}.schema_hash"))
        .await
    {
        Ok(got) if got == want => None,
        Ok(got) => Some(Mismatch::Stale {
            database: database.to_owned(),
            want,
            got: got.to_string(),
        }),
        Err(_) => Some(Mismatch::Stale {
            database: database.to_owned(),
            want,
            got: "not set".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_names_the_database_the_table_and_the_column() {
        let mismatch = Mismatch::Changed {
            database: "nat".to_owned(),
            table: "native_state".to_owned(),
            column: "unresolved".to_owned(),
            want: "UInt64".to_owned(),
            got: "UInt8".to_owned(),
        };
        assert_eq!(
            mismatch.to_string(),
            "nat.native_state.unresolved is UInt8, schema.sql declares UInt64. \
             Pass --fresh to drop and reload"
        );
    }

    #[test]
    fn a_stale_hash_names_the_database_and_both_hashes() {
        let mismatch = Mismatch::Stale {
            database: "nat".to_owned(),
            want: 42,
            got: "not set".to_owned(),
        };
        assert_eq!(
            mismatch.to_string(),
            "nat's schema hash is not set, this binary's own is 42. \
             Load it fresh with this binary before reading it"
        );
    }
}
