//! DOOM's per-tic simulation, as SQL text.
//!
//! Each module here transliterates one engine file, function for function,
//! into expressions over the state row. Nothing executes: a caller gets
//! statements and issues them.

pub mod doors;
pub mod enemy;
pub mod game;
pub mod hud;
pub mod inter;
pub mod lights;
pub mod map;
pub mod maputl;
pub mod mobj;
pub mod noise;
pub mod plane;
pub mod plats;
pub mod player;
pub mod pspr;
pub mod setup;
pub mod shoot;
pub mod sight;
pub mod spec;
pub mod specials;
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
    statements.extend(enemy::guards(db));
    statements.extend(player::guards(db));
    statements.extend(noise::guards(db));
    statements.extend(mobj::guards(db));
    statements.extend(setup::statements(db));
    statements
}

/// The newest binding holding each state column as a tic builds up.
///
/// The engine's order is total: a function reads whatever the last
/// function to write a field left there, and more than one function may
/// write the same field in a tic. A stage names what it computes
/// `now_<column>` and the tic renames it to that stage's own binding, so a
/// later stage reads the newest value and the row reads the last.
#[derive(Default)]
pub struct State {
    written: Vec<(&'static str, String)>,
}

impl State {
    /// The binding holding `column` at this point in the tic.
    pub fn get(&self, column: &str) -> String {
        self.written
            .iter()
            .rev()
            .find(|(written, _)| *written == column)
            .map(|(_, binding)| binding.clone())
            .unwrap_or_else(|| format!("prev_{column}"))
    }
}

/// A tic's bindings as its stages are added, and what each column holds.
pub struct Tic {
    pub state: State,
    bindings: Vec<(String, String)>,
    stages: usize,
}

impl Tic {
    fn new(bindings: Vec<(String, String)>) -> Tic {
        Tic {
            state: State::default(),
            bindings,
            stages: 0,
        }
    }

    /// Adds one stage's bindings, which every later stage may read.
    fn stage(&mut self, bindings: Vec<(String, String)>) {
        let at = self.stages;
        self.stages += 1;
        let mut bindings = bindings;
        for index in 0..bindings.len() {
            let (name, expr) = bindings[index].clone();
            let Some(column) = column_of(&name) else {
                self.bindings.push((name, expr));
                continue;
            };
            let binding = format!("s{at}_{column}");
            self.bindings.push((binding.clone(), expr));
            self.state.written.push((column, binding.clone()));
            // A stage names its own results the way every stage does, so
            // the ones after it in the same stage are pointed at the name
            // this one landed under.
            for (_, later) in bindings.iter_mut().skip(index + 1) {
                *later = rename(later, &name, &binding);
            }
        }
    }

    /// Adds a stage that only runs while `running` holds, which is how the
    /// engine's early returns read from outside the function.
    ///
    /// Every state column the stage computes lands under a second name and
    /// the column itself picks between that and what it held before.
    fn stage_when(&mut self, running: &str, bindings: Vec<(String, String)>) {
        let at = self.stages;
        let mut gated = Vec::new();
        for (name, expr) in bindings {
            match column_of(&name) {
                Some(column) => {
                    let held = format!("ran{at}_{column}");
                    let unless = self.state.get(column);
                    gated.push((held.clone(), expr));
                    gated.push((name, format!("if({running}, {held}, {unless})")));
                }
                None => gated.push((name, expr)),
            }
        }
        self.stage(gated);
    }
}

/// `expr` with every whole-word use of `from` replaced by `to`.
fn rename(expr: &str, from: &str, to: &str) -> String {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(expr.len());
    let mut rest = expr;
    while let Some(at) = rest.find(from) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + from.len()..].chars().next();
        out.push_str(&rest[..at]);
        if before.is_some_and(ident) || after.is_some_and(ident) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[at + from.len()..];
    }
    out.push_str(rest);
    out
}

/// The state column a stage's binding writes, if it writes one.
fn column_of(name: &str) -> Option<&'static str> {
    let column = name.strip_prefix("now_")?;
    state_columns().into_iter().find(|known| *known == column)
}

/// The engine tables more than one stage reads, as constant arrays indexed
/// by id plus one.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let mut constants = vec![
        ("rnd".to_owned(), table_column(db, "rndtable", "value")),
        (
            "tantoangle".to_owned(),
            table_column(db, "tantoangle", "value"),
        ),
        (
            "line_side0".to_owned(),
            table_column(db, "lv_lines", "side0"),
        ),
        ("state_tics".to_owned(), table_column(db, "states", "tics")),
        (
            "state_sprite".to_owned(),
            table_column(db, "states", "sprite"),
        ),
        (
            "state_frame".to_owned(),
            table_column(db, "states", "frame"),
        ),
        (
            "state_nextstate".to_owned(),
            table_column(db, "states", "nextstate"),
        ),
        (
            "state_action".to_owned(),
            table_column(db, "states", "action"),
        ),
    ];
    constants.extend(maputl::constants(db));
    constants.extend(plane::constants(db));
    constants.extend(inter::constants(db));
    constants.extend(player::constants(db));
    constants.extend(pspr::constants(db));
    constants.extend(sight::constants(db));
    constants.extend(shoot::constants(db));
    constants.extend(inter::damage_constants(db));
    constants.extend(enemy::constants(db));
    constants.extend(mobj::constants(db));
    constants
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
/// The bindings are one flat `WITH` list, which is what a statement whose
/// bindings do not chain wants.
fn insert_flat(db: &str, with: &[(String, String)], row: &[(&str, String)], from: &str) -> String {
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
            format!("    ({expr}) AS {name}")
        })
        .collect();
    let bindings: Vec<String> = with
        .iter()
        .map(|(name, expr)| format!("    ({expr}) AS {name}"))
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

/// `INSERT INTO {db}.native_state (...) SELECT ... FROM ...`, with the
/// bindings staged as nested subqueries.
///
/// A binding named in another binding's text is expanded into it rather
/// than shared, so a chain of aliases grows the query tree by the product
/// of its branches. A subquery's column is not expanded, so the bindings
/// are cut into stages before a binding that would copy too much of what
/// it names, and each stage is one subquery over the one below it.
///
/// `row` gives one expression per state column, in any order; the insert
/// names its columns and emits them in the contract's order, so a column
/// nobody wrote is a panic here rather than a wrong row in the table.
fn insert(
    db: &str,
    with: &[(String, String)],
    row: &[(&str, String)],
    from: &str,
    carried: &[&str],
) -> String {
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
            format!("    ({expr}) AS {name}")
        })
        .collect();
    format!(
        "INSERT INTO {db}.native_state\n(\n{}\n)\nSELECT\n{}\nFROM\n(\n{}\n)",
        columns
            .iter()
            .map(|name| format!("    {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        select.join(",\n"),
        indent(&nest(&stages(with), from, &select.join(" "), carried)),
    )
}

/// How many bytes of the bindings it names a binding may copy before the
/// stage is cut ahead of it.
///
/// The analyser walks what a binding copies as well as what it holds, and
/// what it costs grows faster than the tree does, so a stage is cut on the
/// copying rather than on the size. Below about 600 the stages the cut
/// makes cost more than the copying they save.
const COPY_LIMIT: usize = 800;

/// The bindings cut into stages, each of which reads only what an earlier
/// stage produced or what it can copy without going past the limit.
fn stages(with: &[(String, String)]) -> Vec<Vec<(String, String)>> {
    let mut stages: Vec<Vec<(String, String)>> = Vec::new();
    let mut current: Vec<(String, String)> = Vec::new();
    // What each binding of the stage holds once the ones it names are
    // expanded into it, which is what naming it copies.
    let mut expanded: Vec<usize> = Vec::new();
    for (name, expr) in with {
        let mut copied = 0;
        for (at, (earlier, _)) in current.iter().enumerate() {
            copied += references(expr, earlier) * expanded[at];
        }
        if copied > COPY_LIMIT && !current.is_empty() {
            stages.push(std::mem::take(&mut current));
            expanded.clear();
            copied = 0;
        }
        current.push((name.clone(), expr.clone()));
        expanded.push(expr.len() + copied);
    }
    if !current.is_empty() {
        stages.push(current);
    }
    stages
}

/// How often `expr` names `binding` as a whole word.
fn references(expr: &str, binding: &str) -> usize {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    expr.match_indices(binding)
        .filter(|(at, _)| {
            let before = expr[..*at].chars().next_back();
            let after = expr[at + binding.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident)
        })
        .count()
}

/// The stages as nested subqueries over `from`, innermost first.
///
/// A stage hands up only what something above it still reads. `SELECT *`
/// would carry every binding to the top, and the analyser resolves the
/// whole list again at each level, which costs more than the stages save.
fn nest(stages: &[Vec<(String, String)>], from: &str, row: &str, carried: &[&str]) -> String {
    // The last stage that reads each binding, counting the row as one
    // past the end.
    let mut last_use: Vec<Vec<usize>> = Vec::new();
    for (at, stage) in stages.iter().enumerate() {
        let mut uses = Vec::new();
        for (name, _) in stage {
            let mut last = at;
            for (later, above) in stages.iter().enumerate().skip(at + 1) {
                if above.iter().any(|(_, expr)| references(expr, name) > 0) {
                    last = later;
                }
            }
            if references(row, name) > 0 {
                last = stages.len();
            }
            uses.push(last);
        }
        last_use.push(uses);
    }

    let mut sql = from.to_owned();
    for (at, stage) in stages.iter().enumerate() {
        let mut columns: Vec<String> = carried.iter().map(|name| format!("    {name}")).collect();
        for (below, earlier) in stages.iter().enumerate().take(at) {
            for (index, (name, _)) in earlier.iter().enumerate() {
                if last_use[below][index] >= at {
                    columns.push(format!("    {name}"));
                }
            }
        }
        columns.extend(
            stage
                .iter()
                .map(|(name, expr)| format!("    ({expr}) AS {name}")),
        );
        sql = if at == 0 {
            format!("SELECT\n{}\nFROM {from}", columns.join(",\n"))
        } else {
            format!(
                "SELECT\n{}\nFROM\n(\n{}\n)",
                columns.join(",\n"),
                indent(&sql)
            )
        };
    }
    sql
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
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

    fn values(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, expr)| ((*name).to_owned(), (*expr).to_owned()))
            .collect()
    }

    /// A binding that copies more than the limit starts a stage, and a
    /// large one that copies nothing does not.
    #[test]
    fn a_stage_is_cut_on_what_a_binding_copies() {
        let big = "x".repeat(COPY_LIMIT + 1);
        let staged = stages(&values(&[("a", &big), ("b", "a + 1")]));
        assert_eq!(staged.len(), 2, "b copies a");
        let staged = stages(&values(&[("a", &big), ("b", &big)]));
        assert_eq!(staged.len(), 1, "neither names the other");
    }

    /// Copying compounds. `b` is five characters and carries two copies
    /// of `a`, so naming it twice copies four, and the cut counts what a
    /// binding holds expanded rather than what its text says.
    #[test]
    fn what_a_binding_copies_counts_what_it_names_already_copied() {
        let a = "x".repeat(COPY_LIMIT / 2 - 100);
        let staged = stages(&values(&[("a", &a), ("b", "a + a"), ("c", "b + b")]));
        assert_eq!(staged.len(), 2);
        assert_eq!(staged[0].len(), 2, "b copies two of a and stays");
        assert_eq!(staged[1].len(), 1, "c copies four and is cut");
    }

    /// Every binding lands in exactly one stage, in the order it was
    /// given, so a stage cannot read one it does not have.
    #[test]
    fn the_stages_hold_every_binding_once_and_in_order() {
        let with = values(&[("a", "1"), ("b", "a"), ("c", "2"), ("d", "b + c")]);
        let staged: Vec<(String, String)> = stages(&with).concat();
        assert_eq!(staged, with);
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
        insert("nat", &[], &row, "(SELECT 1)", &[]);
    }
}
