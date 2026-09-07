//! A state row written by hand, so a test can put something on the list
//! that `demo3` does not reach.
//!
//! The row is a copy of one the run already made, with named columns
//! replaced. `native_state` is a `Join` table and ClickHouse will not let
//! one query read and write it, so the copy goes through a table of its
//! own.

use clickdoom_native::sql::sim;

/// The statements that write a copy of the row at `from` under `tic`, with
/// each named column replaced by its expression. An expression may name
/// the source row's columns through the `p` alias.
pub fn row(db: &str, tic: u32, from: u32, overrides: &[(&str, String)]) -> Vec<String> {
    let mut named: Vec<&str> = Vec::new();
    for (column, _) in overrides {
        assert!(
            !named.contains(column),
            "{column} is overridden twice; a column two slots both touch \
             needs one combined expression, not two separate ones, since \
             only the first would ever apply"
        );
        named.push(column);
    }
    let columns: Vec<String> = sim::state_columns()
        .into_iter()
        .map(|column| {
            let value = if column == "tic" {
                format!("toUInt32({tic})")
            } else if let Some((_, value)) = overrides.iter().find(|(name, _)| *name == column) {
                value.clone()
            } else {
                format!("p.{column}")
            };
            format!("    {value} AS {column}")
        })
        .collect();
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS {db}.seed_row ENGINE = Memory AS \
             SELECT * FROM {db}.native_state WHERE tic = {from}"
        ),
        format!(
            "INSERT INTO {db}.native_state\nSELECT\n{}\nFROM {db}.seed_row AS p",
            columns.join(",\n")
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::row;

    /// Two separate overrides of the same column used to mean the second
    /// silently lost to `find`'s own first match, seeding the wrong row
    /// rather than failing the test that read it.
    #[test]
    #[should_panic(expected = "m_x is overridden twice")]
    fn a_column_overridden_twice_panics() {
        row(
            "db",
            1,
            0,
            &[("m_x", "1".to_owned()), ("m_x", "2".to_owned())],
        );
    }
}
