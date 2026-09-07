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

/// The column list inside a `CREATE TABLE`'s outer parens, as one string
/// with every comment already stripped: a trailing `-- comment` loses
/// everything from the `--` on, and a standalone comment line loses all of
/// it, both before anything here looks for a comma.
///
/// Stripping first, over the whole list at once, is what keeps a comment
/// carrying its own commas (`bbox`'s `-- top, bottom, left, right`) from
/// being counted as column separators.
fn strip_comments(body: &str) -> String {
    body.lines()
        .map(|line| line.split("--").next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text`, with every run of whitespace collapsed to one space and no
/// space just inside a paren: the one canonical form both a type
/// `native/schema.sql` spans several lines to declare, and the type a
/// live server's `system.columns` names back, reduce to.
///
/// A schema type collapses to this shape by construction; a value read
/// off a server already carries it, but normalising both sides before a
/// comparison, rather than trusting the server never to differ, is what
/// tells a real mismatch from a rendering one.
pub fn collapse_type(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace(" )", ")")
}

/// `text`, split on every comma at paren depth 0.
///
/// A column's own type can carry parens deeper than that (`Array(Int32)`,
/// or a `Tuple` whose members run over several lines with commas of their
/// own), and those belong to the entry the comma sits inside, not to the
/// list this splits.
fn top_level_entries(text: &str) -> Vec<&str> {
    let mut depth = 0i32;
    let mut start = 0;
    let mut entries = Vec::new();
    for (at, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                entries.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    entries.push(&text[start..]);
    entries
}

/// Every column the schema declares, as `(table, column, type)`, in
/// declaration order. A type that spans several lines (a `Tuple`'s own
/// members, one per line) comes back with its whitespace collapsed to
/// single spaces, matching how `system.columns` renders one back; `column`
/// is owned for the same reason `type` is, since both are read out of a
/// comment-stripped copy of the schema text rather than the text itself.
///
/// `system.columns` names a loaded database's own columns and types the
/// same way, so a caller can compare the two directly rather than trusting
/// `CREATE TABLE IF NOT EXISTS` to notice a schema that moved underneath a
/// table that already exists.
pub fn schema_columns() -> Vec<(&'static str, String, String)> {
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
        let stripped = strip_comments(&body[..close]);
        for entry in top_level_entries(&stripped) {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let Some((column, kind)) = entry.split_once(char::is_whitespace) else {
                continue;
            };
            columns.push((table, column.to_owned(), collapse_type(kind)));
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
fn native_state_types() -> Vec<(String, String)> {
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
            columns.contains(&("native_state", "unresolved".to_owned(), "UInt64".to_owned())),
            "{columns:?}"
        );
        assert!(
            columns.contains(&("finetangent", "value".to_owned(), "Int32".to_owned())),
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

    /// `native_frames.st_cache` is a `Tuple` whose members run one per
    /// line, each with a comma of its own: exactly the shape a column
    /// list split on every comma, rather than only the ones between
    /// columns, would tear apart into `st_cache`'s own members.
    #[test]
    fn a_multi_line_tuple_column_is_named_once_with_its_whole_type() {
        let columns = schema_columns();
        let st_cache: Vec<_> = columns
            .iter()
            .filter(|(table, column, _)| *table == "native_frames" && *column == "st_cache")
            .collect();
        assert_eq!(st_cache.len(), 1, "{columns:?}");
        assert_eq!(
            st_cache[0].2,
            "Tuple(ready Int32, frags Int32, health Int32, armor Int32, \
             ammo Array(Int32), maxammo Array(Int32), arms Array(Int32), \
             keyboxes Array(Int32), faceindex Int32, armsbg Int32)",
            "collapsing has to match system.columns' own rendering exactly, \
             not just start the same way"
        );
        assert!(
            columns
                .iter()
                .all(|(table, column, _)| !(*table == "native_frames"
                    && ["ready", "frags", "health", "armor", "faceindex", "armsbg"]
                        .contains(&column.as_str()))),
            "a tuple member split out as its own column: {columns:?}"
        );
    }

    #[test]
    fn collapse_type_matches_a_named_tuple_s_own_rendering() {
        assert_eq!(collapse_type("Int32"), "Int32");
        assert_eq!(collapse_type("  Int32  "), "Int32");
        assert_eq!(
            collapse_type("Tuple(\n  ready  Int32,\n  frags  Int32\n)"),
            "Tuple(ready Int32, frags Int32)"
        );
        // A nested `Array(...)` carries no whitespace of its own to begin
        // with, so it passes through unchanged.
        assert_eq!(collapse_type("Array(Int32)"), "Array(Int32)");
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
