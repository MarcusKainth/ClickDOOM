//! The statements native mode runs, as text and bytes.
//!
//! Nothing here executes. A caller gets a list of [`Statement`]s and issues
//! them; that is the whole of the driver's part in loading a level.

pub mod bind;
pub mod bsp;
pub mod fixed;
pub mod parity;
pub mod probe;
pub mod render;
pub mod rowbinary;
pub mod sim;
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

/// Every table the schema declares, in the order it declares them.
///
/// A caller that has to empty or drop the schema's tables one at a time
/// needs the list, and the schema is the only place it exists.
pub fn schema_tables() -> Vec<&'static str> {
    let prefix = format!("CREATE TABLE IF NOT EXISTS {DB_PLACEHOLDER}.");
    split_statements(SCHEMA)
        .into_iter()
        .filter_map(|sql| sql.strip_prefix(prefix.as_str()))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|name| name.trim_end_matches('('))
        .collect()
}

/// Every column the schema declares, as `(table, column, type)`, in
/// declaration order.
///
/// `system.columns` names a loaded database's own columns and types the
/// same way, so a caller can compare the two directly rather than trusting
/// `CREATE TABLE IF NOT EXISTS` to notice a schema that moved underneath a
/// table that already exists.
pub fn schema_columns() -> Vec<(&'static str, &'static str, &'static str)> {
    let prefix = format!("CREATE TABLE IF NOT EXISTS {DB_PLACEHOLDER}.");
    let mut columns = Vec::new();
    for sql in split_statements(SCHEMA) {
        let Some(rest) = sql.strip_prefix(prefix.as_str()) else {
            continue;
        };
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(rest.len());
        let table = &rest[..name_end];
        let open = rest
            .find('(')
            .expect("a CREATE TABLE names its columns in parens");
        let body = &rest[open + 1..];
        let mut depth = 1i32;
        let mut close = body.len();
        for (at, byte) in body.bytes().enumerate() {
            match byte {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = at;
                        break;
                    }
                }
                _ => {}
            }
        }
        for line in body[..close].lines() {
            let line = line.split("--").next().unwrap_or_default();
            for entry in line.split(',') {
                let entry = entry.trim();
                let Some((column, kind)) = entry.split_once(char::is_whitespace) else {
                    continue;
                };
                columns.push((table, column, kind.trim()));
            }
        }
    }
    columns
}

/// This schema's own identity: `xxh64` of the DDL text, seeded at 0.
///
/// `native load` writes this into `schema_hash` the moment a load
/// finishes; a session refuses a database whose row does not match,
/// naming both, rather than reading state rows a different schema wrote.
pub fn schema_hash() -> u64 {
    xxhash_rust::xxh64::xxh64(SCHEMA.as_bytes(), 0)
}

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
    schema_columns()
        .into_iter()
        .filter(|(table, _, _)| *table == "native_state")
        .map(|(_, column, kind)| (column, kind))
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
    fn the_table_list_names_every_create_the_schema_carries() {
        let tables = schema_tables();
        let creates = schema_statements("nat")
            .iter()
            .filter(|s| s.sql.starts_with("CREATE TABLE"))
            .count();
        assert_eq!(tables.len(), creates, "a CREATE TABLE has no name here");
        assert!(tables.contains(&"wad_lumps"));
        assert!(tables.contains(&"native_state"));
        assert!(tables.contains(&"native_frames"));
        assert!(tables.contains(&"melt_schedule"));
        // A name that kept its opening bracket would not truncate.
        assert!(
            tables.iter().all(|t| !t.contains('(') && !t.contains('.')),
            "{tables:?}"
        );
    }

    #[test]
    fn every_column_names_the_table_the_schema_declares_it_on() {
        let columns = schema_columns();
        let tables: std::collections::HashSet<_> =
            columns.iter().map(|(table, _, _)| *table).collect();
        assert_eq!(
            tables,
            schema_tables().into_iter().collect(),
            "a table with no columns, or a column with no table, went missing"
        );
        assert!(
            columns.contains(&("native_state", "unresolved", "UInt64")),
            "{columns:?}"
        );
        assert!(
            columns.contains(&("finetangent", "value", "Int32")),
            "a single-line CREATE TABLE parses too: {columns:?}"
        );
        // A comment on its own line, or trailing a column's, names no
        // column and carries no comma into the split.
        assert!(
            columns
                .iter()
                .all(|(_, column, _)| !column.is_empty() && !column.starts_with("--")),
            "{columns:?}"
        );
    }

    #[test]
    fn the_hash_is_the_same_text_hashed_the_same_way_each_time() {
        let hash = schema_hash();
        assert_ne!(hash, 0);
        assert_eq!(hash, schema_hash());
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
