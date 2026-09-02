//! The reference emulator's state rows, and the table they load into.
//!
//! `probe_state` carries one row per frame commit in `native_state`'s shape,
//! with the frame index, the engine's `gametic` and the frame hash in front.
//! Its column types come from the `native_state` declaration in
//! `native/schema.sql`, so the two tables cannot drift apart.

use clickdoom_spec::native_state;

use super::Statement;

/// Columns the probe writes ahead of the contract's field list.
/// `fb_hash` is a `String` because the probe writes it as 16 hex digits.
const LEADING: [(&str, &str); 3] = [
    ("frame_index", "UInt32"),
    ("gametic", "UInt32"),
    ("fb_hash", "String"),
];

/// A probe file whose header does not name the columns the contract does.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the probe file has no `# columns` header")]
    NoHeader,
    #[error("the probe file names {found} columns, the contract has {want}")]
    ColumnCount { found: usize, want: usize },
    #[error("the probe file's column {at} is {found}, the contract has {want}")]
    ColumnName {
        at: usize,
        found: String,
        want: String,
    },
}

/// `probe_state` in `db`, keyed by frame index.
pub fn schema_statement(db: &str) -> Statement {
    let columns: Vec<String> = columns()
        .into_iter()
        .map(|(name, kind)| format!("    {name} {kind}"))
        .collect();
    Statement::sql(format!(
        "CREATE TABLE IF NOT EXISTS {db}.probe_state\n(\n{}\n)\nENGINE = MergeTree ORDER BY frame_index",
        columns.join(",\n")
    ))
}

/// The insert that loads one probe TSV, with the file as its body.
///
/// The file opens with comment lines the server skips. The last of them
/// names the columns, and it has to be the contract's list in order,
/// because the rows that follow are positional.
pub fn insert(db: &str, tsv: &str) -> Result<Statement, Error> {
    let comments = tsv.lines().take_while(|line| line.starts_with('#')).count();
    check_header(tsv)?;
    Ok(Statement::data(
        format!(
            "INSERT INTO {db}.probe_state \
             SETTINGS input_format_tsv_skip_first_lines = {comments} FORMAT TSV"
        ),
        tsv.as_bytes().to_vec(),
    ))
}

/// The `# columns` header against the contract, column by column.
fn check_header(tsv: &str) -> Result<(), Error> {
    let header = tsv
        .lines()
        .take_while(|line| line.starts_with('#'))
        .find_map(|line| line.strip_prefix("# columns\t"))
        .ok_or(Error::NoHeader)?;
    let found: Vec<&str> = header.split('\t').collect();
    let want: Vec<&str> = names();
    if found.len() != want.len() {
        return Err(Error::ColumnCount {
            found: found.len(),
            want: want.len(),
        });
    }
    for (at, (found, want)) in found.iter().zip(&want).enumerate() {
        if found != want {
            return Err(Error::ColumnName {
                at,
                found: (*found).to_owned(),
                want: (*want).to_owned(),
            });
        }
    }
    Ok(())
}

/// Every `probe_state` column, in the order the probe writes them.
pub fn names() -> Vec<&'static str> {
    columns().into_iter().map(|(name, _)| name).collect()
}

/// Every `probe_state` column with its type, the contract's fields taking
/// the type `native_state` declares for them.
fn columns() -> Vec<(&'static str, &'static str)> {
    let declared = super::native_state_types();
    let mut columns = LEADING.to_vec();
    for field in native_state::all_fields() {
        let kind = declared
            .iter()
            .find(|(name, _)| *name == field)
            .map(|(_, kind)| *kind)
            .unwrap_or_else(|| panic!("native_state declares no column {field}"));
        columns.push((field, kind));
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture's own header, which is the shape every probe file has.
    const HEADER: &str = "# refemu-probe 1\n# state_schema_version\t1\n";

    fn file(columns: &[&str]) -> String {
        format!("{HEADER}# columns\t{}\n0\t1\n", columns.join("\t"))
    }

    #[test]
    fn the_table_carries_the_probe_columns_in_order() {
        let sql = schema_statement("nat").sql;
        assert!(sql.starts_with("CREATE TABLE IF NOT EXISTS nat.probe_state"));
        assert!(sql.contains("    frame_index UInt32,\n    gametic UInt32,\n    fb_hash String,"));
        assert!(sql.contains("    leveltime Int32,"));
        assert!(sql.contains("    m_player Array(Int8),"));
        assert!(sql.ends_with("ENGINE = MergeTree ORDER BY frame_index"));
    }

    #[test]
    fn every_contract_field_has_a_type_the_schema_declares() {
        let declared = super::super::native_state_types();
        for field in native_state::all_fields() {
            assert!(
                declared.iter().any(|(name, _)| *name == field),
                "native_state declares no {field}"
            );
        }
    }

    #[test]
    fn a_file_that_names_the_contract_loads() {
        let tsv = file(&names());
        let statement = insert("nat", &tsv).unwrap();
        assert_eq!(
            statement.sql,
            "INSERT INTO nat.probe_state \
             SETTINGS input_format_tsv_skip_first_lines = 3 FORMAT TSV"
        );
        assert_eq!(statement.body, tsv.as_bytes());
    }

    #[test]
    fn a_file_whose_columns_moved_is_rejected_naming_both() {
        let mut columns = names();
        columns.swap(3, 4);
        let error = insert("nat", &file(&columns)).unwrap_err();
        assert!(matches!(error, Error::ColumnName { at: 3, .. }), "{error}");
        assert!(error.to_string().contains("leveltime"), "{error}");
    }

    #[test]
    fn a_file_with_no_header_is_rejected() {
        assert!(matches!(insert("nat", "0\t1\n"), Err(Error::NoHeader)));
        let short = &names()[..4];
        assert!(matches!(
            insert("nat", &file(short)),
            Err(Error::ColumnCount { found: 4, .. })
        ));
    }
}
