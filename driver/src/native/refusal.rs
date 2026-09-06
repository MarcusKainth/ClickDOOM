//! Whether a run ever left a tic unresolved.
//!
//! `NATIVE.md`'s parity section states the contract: `native_state`'s
//! `unresolved` and `unimplemented` columns say a tic could not be
//! produced exactly, and a caller stops there rather than comparing or
//! rendering it like any other.

use std::fmt;

use clickhouse::Row;
use serde::Deserialize;

use crate::client::{Db, Error};

/// The tic a run stopped at, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub tic: u32,
    pub reason: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tic {} {}", self.tic, self.reason)
    }
}

#[derive(Row, Deserialize)]
struct RefusalRow {
    tic: u32,
    unimplemented: u64,
}

/// The first tic up to and including `upto` whose row sets `unresolved` or
/// `unimplemented`, if any.
///
/// `unimplemented` is decided once, when the level loads, so a level that
/// sets it refuses from tic 1 on; `unresolved` is decided fresh each tic.
/// A tic that sets both names `unimplemented`, since nothing after it can
/// be produced either.
pub async fn first(db: &Db, database: &str, upto: u32) -> Result<Option<Refusal>, Error> {
    let sql = format!(
        "SELECT tic, unimplemented FROM {database}.native_state \
         WHERE tic <= {upto} AND (unresolved = 1 OR unimplemented != 0) \
         ORDER BY tic LIMIT 1"
    );
    let rows: Vec<RefusalRow> = db.fetch_all(&sql).await?;
    Ok(rows.into_iter().next().map(|row| Refusal {
        tic: row.tic,
        reason: reason(row.unimplemented),
    }))
}

fn reason(unimplemented: u64) -> String {
    if unimplemented != 0 {
        format!(
            "unimplemented: {}",
            clickdoom_native::sql::sim::unimplemented_names(unimplemented)
        )
    } else {
        "unresolved".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unimplemented_names_the_tic_and_plain_unresolved_does_not() {
        assert_eq!(reason(0), "unresolved");
        assert_eq!(
            reason(clickdoom_native::sql::sim::unimplemented::SECTOR_DOOR),
            "unimplemented: SECTOR_DOOR"
        );
    }

    #[test]
    fn the_message_names_the_tic_and_the_column() {
        let refusal = Refusal {
            tic: 111,
            reason: "unresolved".to_owned(),
        };
        assert_eq!(refusal.to_string(), "tic 111 unresolved");

        let refusal = Refusal {
            tic: 1,
            reason: "unimplemented: SECTOR_DOOR".to_owned(),
        };
        assert_eq!(refusal.to_string(), "tic 1 unimplemented: SECTOR_DOOR");
    }
}
