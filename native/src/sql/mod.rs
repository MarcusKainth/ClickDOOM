//! The statements native mode runs, as text and bytes.
//!
//! Nothing here executes. A caller gets a list of [`Statement`]s and issues
//! them; that is the whole of the driver's part in loading a level.

pub mod bsp;
pub mod fixed;
pub mod parity;
pub mod probe;
pub mod render;
pub mod rowbinary;
pub mod statement;

pub use statement::{Statement, split_statements};

use crate::tables;

/// The DDL, with `{{DB}}` still in it.
const SCHEMA: &str = include_str!("../../schema.sql");

/// The level decode, with its three placeholders still in it.
const LEVEL_LOAD: &str = include_str!("../../sql/level_load.sql");

/// The renderer's own tables, with its two placeholders still in it.
const RENDER_LOAD: &str = include_str!("../../sql/render_load.sql");

/// The database name placeholder every generated statement carries.
const DB_PLACEHOLDER: &str = "{{DB}}";

/// The schema as one statement per `CREATE`, against `db`.
pub fn schema_statements(db: &str) -> Vec<Statement> {
    split_statements(&SCHEMA.replace(DB_PLACEHOLDER, db))
        .into_iter()
        .map(Statement::sql)
        .collect()
}

/// The level decode, one statement at a time, for `map` in `db` driven by
/// the demo lump `demo`.
///
/// Every statement reads `wad_lumps` and writes a derived table, so the
/// database has to carry a loaded WAD already. A statement that starts
/// `SELECT throwIf` is a guard: it returns a row, and it fails the load
/// when the thing it checks is wrong.
pub fn level_statements(db: &str, map: &str, demo: &str) -> Vec<Statement> {
    let text = LEVEL_LOAD
        .replace(DB_PLACEHOLDER, db)
        .replace("{{MAP}}", map)
        .replace("{{DEMO}}", demo);
    split_statements(&text)
        .into_iter()
        .map(Statement::sql)
        .collect()
}

/// The renderer's own tables, one statement at a time, for the episode whose
/// sky texture is `sky`.
///
/// Every statement reads a constant table or a decoded level table, so the
/// database has to carry a loaded level already.
pub fn render_statements(db: &str, sky: &str) -> Vec<Statement> {
    let text = RENDER_LOAD
        .replace(DB_PLACEHOLDER, db)
        .replace("{{SKY}}", sky);
    split_statements(&text)
        .into_iter()
        .map(Statement::sql)
        .collect()
}

/// The insert that loads a WAD's lumps, and its rows as RowBinary.
///
/// The rows travel as the request body, so the statement stays short
/// whatever the WAD's size. Column order is the statement's, not the
/// table's.
pub fn wad_insert(db: &str, wad: &crate::wad::Wad<'_>) -> Statement {
    let mut body = Vec::new();
    for lump in wad.lumps() {
        rowbinary::u32(&mut body, lump.index);
        rowbinary::string(&mut body, lump.name.as_bytes());
        rowbinary::string(&mut body, lump.map_marker.as_bytes());
        rowbinary::string(&mut body, lump.bytes);
    }
    Statement::data(
        format!("INSERT INTO {db}.wad_lumps (id, name, map_marker, bytes) FORMAT RowBinary"),
        body,
    )
}

/// The columns `native_state` declares, name and type, in declaration order.
///
/// The schema is the one place that says what type a state column has.
/// `probe_state` takes its types from here, so the table the probe loads
/// into and the table the simulation writes cannot disagree on a type.
fn native_state_types() -> Vec<(&'static str, &'static str)> {
    let (_, rest) = SCHEMA
        .split_once("CREATE TABLE IF NOT EXISTS {{DB}}.native_state\n(\n")
        .expect("the schema declares native_state");
    let (body, _) = rest
        .split_once("\n)\nENGINE")
        .expect("the native_state declaration ends with its engine");
    body.lines()
        .map(|line| line.split("--").next().unwrap_or_default().trim())
        .map(|line| line.trim_end_matches(','))
        .filter_map(|line| line.split_once(char::is_whitespace))
        .map(|(name, kind)| (name, kind.trim()))
        .collect()
}

/// One insert per constant table, streaming the committed TSV.
pub fn table_insert_statements(db: &str) -> Vec<Statement> {
    tables::insert_statements(db)
        .into_iter()
        .map(|insert| Statement::data(insert.sql, insert.body.as_bytes().to_vec()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_names_the_database_everywhere() {
        let statements = schema_statements("nat");
        assert!(statements.len() > 40);
        assert!(
            statements
                .iter()
                .all(|s| !s.sql.contains(DB_PLACEHOLDER) && s.body.is_empty())
        );
        assert_eq!(statements[0].sql, "CREATE DATABASE IF NOT EXISTS nat");
        assert!(
            statements[1]
                .sql
                .starts_with("CREATE TABLE IF NOT EXISTS nat.wad_lumps")
        );
    }

    #[test]
    fn every_table_the_schema_declares_is_named_after_the_database() {
        // A `CREATE TABLE` that forgot its `{{DB}}` would land in whatever
        // database the connection defaults to, which is the shared one.
        for statement in schema_statements("nat") {
            if let Some(rest) = statement.sql.strip_prefix("CREATE TABLE IF NOT EXISTS ") {
                assert!(rest.starts_with("nat."), "{rest}");
            }
        }
    }

    #[test]
    fn the_table_inserts_carry_the_committed_text() {
        let inserts = table_insert_statements("nat");
        assert_eq!(inserts.len(), tables::TABLES.len());
        assert_eq!(inserts[0].sql, "INSERT INTO nat.states FORMAT TSVWithNames");
        assert_eq!(inserts[0].body, tables::TABLES[0].tsv.as_bytes());
    }
}
