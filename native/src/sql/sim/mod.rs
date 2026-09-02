//! DOOM's per-tic simulation, as SQL text.
//!
//! Each module here transliterates one engine file, function for function,
//! into expressions over the state row. Nothing executes: a caller gets
//! statements and issues them.

pub mod setup;

use clickdoom_spec::native_state;

/// Columns `native_state` carries beyond the contract's field list.
const EXTRA_FIELDS: [&str; 4] = ["unresolved", "unimplemented", "dbg_ran", "dbg_prnd"];

/// Bits of `native_state.unimplemented`. A tic that reaches one of these
/// paths sets its bit, and the run stops.
pub mod unimplemented {
    /// A sector special that spawns a door thinker.
    pub const SECTOR_DOOR: u64 = 1 << 0;
}

/// Every column a state row carries, in `native_state`'s order.
pub fn state_columns() -> Vec<&'static str> {
    let mut columns = vec!["tic"];
    columns.extend(native_state::all_fields());
    columns.extend(EXTRA_FIELDS);
    columns
}

/// `INSERT INTO {db}.native_state (...) WITH ... SELECT ... FROM ...`.
///
/// `row` gives one expression per state column, in any order; the insert
/// names its columns and emits them in the contract's order, so a column
/// nobody wrote is a panic here rather than a wrong row in the table.
fn insert(db: &str, with: &[(&str, String)], row: &[(&str, String)], from: &str) -> String {
    let columns = state_columns();
    assert_eq!(
        row.len(),
        columns.len(),
        "the row does not fill every column"
    );
    let select: Vec<String> = columns
        .iter()
        .map(|name| {
            let (_, expr) = row
                .iter()
                .find(|(column, _)| column == name)
                .unwrap_or_else(|| panic!("no expression for {name}"));
            format!("    {expr} AS {name}")
        })
        .collect();
    let bindings: Vec<String> = with
        .iter()
        .map(|(name, expr)| format!("    {expr} AS {name}"))
        .collect();
    format!(
        "INSERT INTO {db}.native_state\n(\n{}\n)\nWITH\n{}\nSELECT\n{}\nFROM\n{from}",
        columns
            .iter()
            .map(|name| format!("    {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        bindings.join(",\n"),
        select.join(",\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_columns_are_the_contract_plus_the_simulation_s_own() {
        let columns = state_columns();
        assert_eq!(columns[0], "tic");
        assert_eq!(columns[1], native_state::GAME_FIELDS[0]);
        assert_eq!(&columns[columns.len() - EXTRA_FIELDS.len()..], EXTRA_FIELDS);
        assert_eq!(columns.len(), native_state::all_fields().len() + 5);
    }

    #[test]
    #[should_panic(expected = "no expression for leveltime")]
    fn a_column_nobody_wrote_is_a_panic_rather_than_a_wrong_row() {
        let row: Vec<(&str, String)> = state_columns()
            .into_iter()
            .map(|name| {
                let name = if name == "leveltime" {
                    "spelt_wrong"
                } else {
                    name
                };
                (name, "0".to_owned())
            })
            .collect();
        insert("nat", &[], &row, "(SELECT 1)");
    }
}
