//! One tic, as the statement a session opens once and feeds a row per tic.
//!
//! `P_Ticker` runs the players, the thinkers and the specials, then
//! `G_Ticker` runs the status bar, the heads-up display and the menu. Each
//! of those is a module here. This one reads the tic before, takes the tic
//! command apart, and names what each stage produced.
//!
//! Every value a stage computes is a `WITH` binding called `now_<column>`,
//! and every column no stage computes is the previous tic's, so the state
//! row is a list of names rather than a list of expressions. Bindings are
//! emitted in the order the engine computes them and each may read the ones
//! before it.
//!
//! The transform is the same whether it runs inside the resident statement
//! or over a single row, so a test issues exactly what a session runs.

use crate::sql::Statement;

use super::{Tic, game, hud, lights, mobj, player, spec, specials, state_columns};

/// The columns the session streams, in wire order. `pad` carries the
/// padding row the transport writes behind the statement text.
pub const INPUT_SCHEMA: &str =
    "tic UInt32, source UInt8, keys UInt32, mouse_dx Int16, mouse_dy Int16, pad String";

/// What a tic command comes from: the demo lump, or the keys and mouse
/// deltas the session streamed.
/// Settings the server has to know before it parses the tic statement.
///
/// The transform is one deeply nested statement, and the defaults for
/// these three are below what it needs. `NATIVE.md` has the rest of the
/// resident session's settings, including the query size, which depends on
/// the statement's own length.
pub const PARSE_SETTINGS: [(&str, &str); 4] = [
    ("max_parser_depth", "20000"),
    ("max_query_size", "8000000"),
    ("max_ast_elements", "4000000"),
    ("max_expanded_ast_elements", "40000000"),
];

/// The columns the input row carries, which every stage passes on.
const INPUT_COLUMNS: &[&str] = &["tic", "source", "keys", "mouse_dx", "mouse_dy"];

pub mod source {
    pub const DEMO: u8 = 0;
    pub const KEYS: u8 = 1;
}

/// `f_wipe.c`: `wipe_initMelt` draws one number per screen column, and the
/// melt runs once, on the tic the first frame is displayed after.
const MELT_DRAWS: u32 = 320;
const MELT_TIC: u32 = 2;

/// The statement a session opens: one row in, one state row out.
///
/// The padding row the transport writes ahead of the first real row
/// carries tic 0, which the filter drops.
pub fn resident_statement(db: &str) -> String {
    transform(db, &format!("input('{INPUT_SCHEMA}')\nWHERE tic > 0"))
}

/// One row a session streams.
pub struct Input {
    pub tic: u32,
    pub source: u8,
    pub keys: u32,
    pub mouse: (i16, i16),
}

impl Input {
    /// A tic the demo lump drives.
    pub fn demo(tic: u32) -> Input {
        Input {
            tic,
            source: source::DEMO,
            keys: 0,
            mouse: (0, 0),
        }
    }

    /// A tic the keys drive.
    pub fn keys(tic: u32, keys: u32, mouse: (i16, i16)) -> Input {
        Input {
            tic,
            source: source::KEYS,
            keys,
            mouse,
        }
    }
}

/// The same transform over a run of input rows, as one statement a caller
/// can issue on its own.
///
/// A row reads the tic before it through `joinGet`, which the `Join`
/// engine makes visible inside the running statement once the rows arrive
/// one block at a time. The settings that make that true are the ones a
/// session sends as URL parameters, and here they travel with the
/// statement. A run is one statement because the transform is analysed
/// per statement and executed per row, exactly as a session's is.
pub fn run_statement(db: &str, rows: &[Input]) -> Statement {
    // The rows come out of `numbers`, which honours the block size the
    // session runs under. A table of literal rows does not, and rows that
    // share a block all read the state from before it.
    let column = |cast: &str, of: &dyn Fn(&Input) -> String| {
        format!(
            "{cast}([{}][1 + number])",
            rows.iter().map(of).collect::<Vec<_>>().join(", ")
        )
    };
    Statement::sql(format!(
        "{}\nSETTINGS max_block_size = 1, max_insert_block_size = 1, \
         min_insert_block_size_rows = 1, min_insert_block_size_bytes = 1, \
         max_threads = 1, max_insert_threads = 1",
        transform(
            db,
            &format!(
                "(\n    SELECT\n        {} AS tic,\n        {} AS source,\n        \
                 {} AS keys,\n        {} AS mouse_dx,\n        {} AS mouse_dy\n    \
                 FROM numbers({})\n)\nWHERE tic > 0",
                column("toUInt32", &|row: &Input| row.tic.to_string()),
                column("toUInt8", &|row: &Input| row.source.to_string()),
                column("toUInt32", &|row: &Input| row.keys.to_string()),
                column("toInt16", &|row: &Input| row.mouse.0.to_string()),
                column("toInt16", &|row: &Input| row.mouse.1.to_string()),
                rows.len()
            ),
        )
    ))
    .with(&PARSE_SETTINGS)
}

/// A run of tics the demo lump drives.
pub fn demo_statement(db: &str, first: u32, last: u32) -> Statement {
    let rows: Vec<Input> = (first..=last).map(Input::demo).collect();
    run_statement(db, &rows)
}

fn transform(db: &str, from: &str) -> String {
    let tic = bindings(db);
    let row = row(&tic.state);
    super::insert(db, &tic.bindings, &row, from, INPUT_COLUMNS)
}

/// Every binding the tic holds, in the order the engine computes them.
///
/// `P_Ticker` runs the players, the thinkers and the specials and then
/// bumps `leveltime`; `G_Ticker` runs the status bar, the heads-up display
/// and the menu after it. The melt comes last because it draws its numbers
/// between the tic and the frame that follows it.
fn bindings(db: &str) -> Tic {
    let mut tic = Tic::new(previous(db));
    tic.stage(super::constants(db));
    let command = game::command(&tic.state, db);
    tic.stage(command);
    let special = game::special_buttons(&tic.state);
    tic.stage(special);
    let think = player::think(&tic.state);
    let running = game::running(&tic.state);
    tic.stage_when(&running, think);
    let things = mobj::thinkers(&tic.state);
    let running = game::running(&tic.state);
    tic.stage_when(&running, things);
    let thinkers = lights::thinkers(&tic.state);
    let running = game::running(&tic.state);
    tic.stage_when(&running, thinkers);
    let planes = specials::planes(&tic.state);
    let running = game::running(&tic.state);
    tic.stage_when(&running, planes);
    let specials = spec::update_specials(&tic.state, db);
    let running = game::running(&tic.state);
    tic.stage_when(&running, specials);
    let running = game::running(&tic.state);
    tic.stage_when(&running, leveltime());
    let tickers = hud::tickers(&tic.state);
    tic.stage(tickers);
    tic.stage(melt());
    tic
}

/// The state row the tic reads, one `joinGet` per column.
///
/// `native_state` is a `Join` table, so each of these is a hash probe
/// against a table held in memory. The tic reads every column, because a
/// column it does not compute it carries forward.
fn previous(db: &str) -> Vec<(String, String)> {
    state_columns()
        .into_iter()
        .filter(|name| *name != "tic")
        .map(|name| {
            (
                format!("prev_{name}"),
                format!("joinGet('{db}.native_state', '{name}', toUInt32(tic - 1))"),
            )
        })
        .collect()
}

/// The last line of `P_Ticker`.
fn leveltime() -> Vec<(String, String)> {
    vec![(
        "now_leveltime".to_owned(),
        "toInt32(prev_leveltime + 1)".to_owned(),
    )]
}

/// What the tic leaves the menu's random index at: `ST_Ticker`'s one draw,
/// and the melt's own on the tic the first frame follows.
fn melt() -> Vec<(String, String)> {
    vec![(
        "now_rndindex".to_owned(),
        format!(
            "toUInt8(bitAnd(toUInt32(prev_rndindex) + 1 + \
             if(tic = {MELT_TIC}, {MELT_DRAWS}, 0), 255))"
        ),
    )]
}

/// Each state column named after the binding that holds it: what the last
/// stage to write it produced, or the previous tic's value.
fn row(state: &super::State) -> Vec<(&'static str, String)> {
    state_columns()
        .into_iter()
        .map(|name| {
            if name == "tic" {
                (name, "tic".to_owned())
            } else {
                (name, state.get(name))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resident_statement_reads_the_session_s_row() {
        let sql = resident_statement("nat");
        assert!(sql.contains(&format!("input('{INPUT_SCHEMA}')")));
        assert!(sql.contains("WHERE tic > 0"));
        assert!(sql.contains("joinGet('nat.native_state', 'leveltime', toUInt32(tic - 1))"));
    }

    /// One walk of the blockmap for the things and one for the lines is
    /// what a single `P_CheckPosition` costs. The statement holds one for
    /// the player's move, one for the chase, and two for the momentum a
    /// thing that is not the player spends, which the engine spends in as
    /// many parts.
    ///
    /// `P_ThingHeightClip` asks a narrower question and has a generator of
    /// its own. The chase's move test sits inside a fold over a list of
    /// one entry or none, so a tic with nothing to chase does not run it,
    /// and both of the momentum's walks read a list that is empty on a tic
    /// where nothing carries any.
    /// `P_XYMovement` returns before the friction for a missile and for a
    /// skull in flight. A move the first cannot make ends it, and one the
    /// second cannot make slams it back into its spawn frames, where
    /// taking friction off either would be wrong. E1M7 holds no skull, so
    /// nothing on the demo reaches this and the statement's own text is
    /// what says the refusal is there.
    #[test]
    fn a_missile_or_a_flying_skull_leaves_the_tic_unresolved() {
        /// `p_mobj.h`: `MF_MISSILE | MF_SKULLFLY`.
        const REFUSED: i64 = 0x1_0000 | 0x100_0000;
        let sql = resident_statement("nat");
        assert!(
            sql.contains(&format!("m_flags[k], {REFUSED}) != 0")),
            "the move refuses both flags together"
        );
        assert!(sql.contains("tx_unrun = 1"), "and the refusal is read");
    }

    #[test]
    fn each_caller_of_the_move_test_holds_one() {
        let sql = resident_statement("nat");
        assert_eq!(sql.matches("arrayMap(mv ->").count(), 5);
        assert_eq!(sql.matches("arrayMap(clip ->").count(), 1);
        assert_eq!(sql.matches("arrayFold((move_at, move_step)").count(), 1);
        assert_eq!(sql.matches("arrayFold((cw_at, cw_step)").count(), 1);
    }

    #[test]
    fn a_step_runs_the_same_transform_over_one_row() {
        let step = run_statement("nat", &[Input::demo(7)]);
        let resident = resident_statement("nat");
        let head = |sql: &str| sql.split("\nFROM\n").next().unwrap().to_owned();
        assert_eq!(head(&step.sql), head(&resident));
        assert!(step.sql.contains("toUInt32([7][1 + number]) AS tic"));
    }

    #[test]
    fn every_column_the_tic_does_not_compute_is_carried_forward() {
        let row = row(&bindings("nat").state);
        assert_eq!(row.len(), state_columns().len());
        let named = |column: &str| {
            row.iter()
                .find(|(name, _)| *name == column)
                .map(|(_, expr)| expr.clone())
                .unwrap()
        };
        assert!(named("s_kind").ends_with("_s_kind"));
        assert!(named("leveltime").ends_with("_leveltime"));
        assert!(!named("leveltime").starts_with("prev_"));
        assert!(!named("st_clock").starts_with("prev_"));
    }

    /// Whether `expr` names `binding`, as a whole word rather than as part
    /// of a longer name.
    fn mentions(expr: &str, binding: &str) -> bool {
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        expr.match_indices(binding).any(|(at, _)| {
            let before = expr[..at].chars().next_back();
            let after = expr[at + binding.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident)
        })
    }

    #[test]
    fn a_binding_is_only_read_after_it_is_written() {
        let with = bindings("nat").bindings;
        let mut written: Vec<&str> = Vec::new();
        for (name, expr) in &with {
            for (earlier, _) in &with {
                if earlier != name && mentions(expr, earlier) {
                    assert!(
                        written.contains(&earlier.as_str()),
                        "{name} reads {earlier} before it is written"
                    );
                }
            }
            written.push(name);
        }
    }

    /// Whether `expr` names `binding` as a name of its own rather than as
    /// the table or column half of a qualified one.
    fn reads(expr: &str, binding: &str) -> bool {
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
        expr.match_indices(binding).any(|(at, _)| {
            let before = expr[..at].chars().next_back();
            let after = expr[at + binding.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident)
        })
    }

    /// Each `(SELECT ...)` in `expr`, brackets balanced, without the
    /// leading bracket.
    fn subqueries(expr: &str) -> Vec<&str> {
        let mut found = Vec::new();
        for (at, _) in expr.match_indices("(SELECT") {
            let mut depth = 0;
            for (offset, c) in expr[at..].char_indices() {
                depth += match c {
                    '(' => 1,
                    ')' => -1,
                    _ => 0,
                };
                if depth == 0 {
                    found.push(&expr[at + 1..at + offset]);
                    break;
                }
            }
        }
        found
    }

    /// A subquery that names a binding from outside itself is a
    /// correlated subquery, which ClickHouse answers with a join. A join
    /// in this statement's pipeline batches the rows a session feeds it
    /// one at a time, so a tic reads the state from before the batch
    /// rather than the tic before it.
    #[test]
    fn a_subquery_names_nothing_bound_outside_it() {
        let with = bindings("nat").bindings;
        let names: Vec<&str> = with.iter().map(|(name, _)| name.as_str()).collect();
        for (name, expr) in &with {
            for query in subqueries(expr) {
                // What the subquery binds for itself, which shadows any
                // name outside it.
                let own: Vec<&str> = query
                    .match_indices(" AS ")
                    .filter_map(|(at, _)| query[at + 4..].split([',', ' ', '\n']).next())
                    .collect();
                for other in &names {
                    assert!(
                        own.contains(other) || !reads(query, other),
                        "the subquery in {name} reads {other} from outside itself"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod expansion {
    use super::*;

    /// The largest bindings are materialised rather than copied.
    ///
    /// A `WITH` binding is copied into every place that names it, and a
    /// binding that names a copied one multiplies again, so a large one
    /// read twice is analysed twice. `stages` cuts a stage ahead of a
    /// binding that would copy too much, which leaves the big ones as
    /// subquery columns that later stages read by name. This fails if one
    /// of them starts being copied instead.
    #[test]
    fn no_large_binding_is_copied() {
        let tic = bindings("lanew");
        let row = row(&tic.state);
        let stages = super::super::stages(&tic.bindings);
        for (at, stage) in stages.iter().enumerate() {
            let last = at + 1 == stages.len();
            let mut count: Vec<usize> = vec![1; stage.len()];
            for index in (0..stage.len()).rev() {
                let (name, _) = &stage[index];
                if last {
                    for (_, expr) in &row {
                        count[index] += super::super::references(expr, name);
                    }
                }
                let mut extra = 0;
                for later in index + 1..stage.len() {
                    extra += super::super::references(&stage[later].1, name) * count[later];
                }
                count[index] += extra;
            }
            for (index, (name, expr)) in stage.iter().enumerate() {
                assert!(
                    expr.len() < 4000 || count[index] == 1,
                    "{name} is {} bytes and copied {} times",
                    expr.len(),
                    count[index]
                );
            }
        }
    }
}
