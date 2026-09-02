//! The statement list that turns an empty database into a loaded one.
//!
//! [`plan`] returns everything to issue, in order. A caller executes them
//! and does nothing else: no decoding, no ordering decisions, no
//! conditionals. That is the whole of the driver's part.

use crate::sql::{self, Statement};
use crate::wad::Wad;

/// Schema, then the raw bytes: the WAD's lumps and the engine's constant
/// tables.
///
/// The plan stops there. Decoding the lumps into level tables is
/// `native/sql/level_load.sql`, which runs against the same database once
/// this has.
pub fn plan(db: &str, wad: &Wad<'_>) -> Vec<Statement> {
    let mut statements = sql::schema_statements(db);
    statements.push(sql::wad_insert(db, wad));
    statements.extend(sql::table_insert_statements(db));
    statements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_comes_before_anything_that_writes_to_it() {
        let bytes = b"IWAD\x00\x00\x00\x00\x0c\x00\x00\x00".to_vec();
        let wad = Wad::parse(&bytes).unwrap();
        let plan = plan("nat", &wad);
        let first_insert = plan
            .iter()
            .position(|s| s.sql.starts_with("INSERT"))
            .expect("the plan inserts something");
        assert!(
            plan[..first_insert]
                .iter()
                .all(|s| s.sql.starts_with("CREATE")),
            "a statement before the first insert is not DDL"
        );
        assert!(plan[first_insert].sql.contains("nat.wad_lumps"));
    }
}
