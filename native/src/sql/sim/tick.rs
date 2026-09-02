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

use super::{Tic, game, hud, player, spec, state_columns};

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
pub const PARSE_SETTINGS: [(&str, &str); 3] = [
    ("max_parser_depth", "20000"),
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
    /// what a single `P_CheckPosition` costs. A second copy of the
    /// primitive would double both.
    #[test]
    fn the_move_test_is_in_the_statement_once() {
        let sql = resident_statement("nat");
        assert_eq!(sql.matches("arrayMap(mv ->").count(), 1);
        assert_eq!(sql.matches("arrayFold((move_at, move_step)").count(), 1);
        assert_eq!(sql.matches("bmap_cols + bx").count(), 2);
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
        assert_eq!(named("s_kind"), "prev_s_kind");
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
}
