//! DOOM's per-tic simulation, as SQL text.
//!
//! Each module here transliterates one engine file, function for function,
//! into expressions over the state row. Nothing executes: a caller gets
//! statements and issues them.

pub mod game;
pub mod hud;
pub mod setup;
pub mod spec;
pub mod tick;

use clickdoom_spec::native_state;

use super::Statement;

/// Columns `native_state` carries beyond the contract's field list.
const EXTRA_FIELDS: [&str; 4] = ["unresolved", "unimplemented", "dbg_ran", "dbg_prnd"];

/// Bits of `native_state.unimplemented`. A tic that reaches one of these
/// paths sets its bit, and the run stops.
pub mod unimplemented {
    /// A sector special that spawns a door thinker.
    pub const SECTOR_DOOR: u64 = 1 << 0;
}

/// Every statement a loaded level needs before its first tic: the guards
/// the engine's own setup stops on, and the row it leaves behind.
pub fn load_statements(db: &str) -> Vec<Statement> {
    let mut statements = spec::guards(db);
    statements.extend(setup::statements(db));
    statements
}

/// The newest binding holding each state column as a tic builds up.
///
/// The engine's order is total: a function reads whatever the last function
/// to write a field left there. A stage reads a column through this, so it
/// names an earlier stage's result where there is one and the previous
/// tic's value where there is not.
#[derive(Default)]
pub struct State {
    written: Vec<&'static str>,
}

impl State {
    /// The binding holding `column` at this point in the tic.
    pub fn get(&self, column: &str) -> String {
        if self.written.contains(&column) {
            format!("now_{column}")
        } else {
            format!("prev_{column}")
        }
    }

    /// Records the state columns a stage's bindings wrote.
    fn wrote(&mut self, bindings: &[(String, String)]) {
        for (name, _) in bindings {
            if let Some(column) = name.strip_prefix("now_")
                && let Some(column) = state_columns().into_iter().find(|c| *c == column)
            {
                self.written.push(column);
            }
        }
    }
}

/// A tic's bindings as its stages are added, and what each column holds.
pub struct Tic {
    pub state: State,
    bindings: Vec<(String, String)>,
}

impl Tic {
    fn new(bindings: Vec<(String, String)>) -> Tic {
        Tic {
            state: State::default(),
            bindings,
        }
    }

    /// Adds one stage's bindings, which every later stage may read.
    fn stage(&mut self, bindings: Vec<(String, String)>) {
        self.state.wrote(&bindings);
        self.bindings.extend(bindings);
    }

    /// Adds a stage that only runs while `running` holds, which is how the
    /// engine's early returns read from outside the function.
    ///
    /// Every state column the stage computes lands under a second name and
    /// the column itself picks between that and what it held before, so a
    /// later stage reading the column sees the value the engine would.
    fn stage_when(&mut self, running: &str, bindings: Vec<(String, String)>) {
        let mut gated = Vec::new();
        for (name, expr) in bindings {
            match name.strip_prefix("now_") {
                Some(column) if state_columns().contains(&column) => {
                    let held = format!("ran_{column}");
                    let unless = self.state.get(column);
                    gated.push((held.clone(), expr));
                    gated.push((name, format!("if({running}, {held}, {unless})")));
                }
                _ => gated.push((name, expr)),
            }
        }
        self.stage(gated);
    }
}

/// The engine tables more than one stage reads, as constant arrays indexed
/// by id plus one.
fn constants(db: &str) -> Vec<(String, String)> {
    vec![
        ("rnd".to_owned(), table_column(db, "rndtable", "value")),
        (
            "tantoangle".to_owned(),
            table_column(db, "tantoangle", "value"),
        ),
        (
            "line_side0".to_owned(),
            table_column(db, "lv_lines", "side0"),
        ),
    ]
}

/// One table column as an array indexed by `id` plus one. The sort is
/// explicit because an aggregate reads its input in whatever order the
/// pipeline hands it over.
fn table_column(db: &str, table: &str, column: &str) -> String {
    format!(
        "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((id, {column}))))\n     \
         FROM {db}.{table})"
    )
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
fn insert(db: &str, with: &[(String, String)], row: &[(&str, String)], from: &str) -> String {
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
