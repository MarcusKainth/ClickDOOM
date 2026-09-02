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

use super::{Tic, game, hud, spec, state_columns};

/// The columns the session streams, in wire order. `pad` carries the
/// padding row the transport writes behind the statement text.
pub const INPUT_SCHEMA: &str =
    "tic UInt32, source UInt8, keys UInt32, mouse_dx Int16, mouse_dy Int16, pad String";

/// What a tic command comes from: the demo lump, or the keys and mouse
/// deltas the session streamed.
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

/// The same transform over one tic, as a statement a caller can issue on
/// its own. `keys` and `mouse` are read only when `source` is
/// [`source::KEYS`].
pub fn step_statement(db: &str, tic: u32, source: u8, keys: u32, mouse: (i16, i16)) -> Statement {
    let (dx, dy) = mouse;
    Statement::sql(transform(
        db,
        &format!(
            "(\n    SELECT\n        toUInt32({tic}) AS tic,\n        \
             toUInt8({source}) AS source,\n        toUInt32({keys}) AS keys,\n        \
             toInt16({dx}) AS mouse_dx,\n        toInt16({dy}) AS mouse_dy\n)\nWHERE tic > 0"
        ),
    ))
}

/// The same transform over a run of tics, as one statement a caller can
/// issue on its own.
///
/// A row reads the tic before it, which `joinGet` makes visible inside the
/// running statement as long as the rows arrive one block at a time. The
/// settings that make that true are the ones a session sends as URL
/// parameters, and here they travel with the statement.
pub fn steps_statement(db: &str, first: u32, last: u32) -> Statement {
    Statement::sql(format!(
        "{}\nSETTINGS max_block_size = 1, max_insert_block_size = 1, \
         min_insert_block_size_rows = 1, min_insert_block_size_bytes = 1, \
         max_threads = 1, max_insert_threads = 1",
        transform(
            db,
            &format!(
                "(\n    SELECT\n        toUInt32(number + {first}) AS tic,\n        \
                 toUInt8({}) AS source,\n        toUInt32(0) AS keys,\n        \
                 toInt16(0) AS mouse_dx,\n        toInt16(0) AS mouse_dy\n    \
                 FROM numbers({})\n)\nWHERE tic > 0",
                source::DEMO,
                u64::from(last) + 1 - u64::from(first)
            ),
        )
    ))
}

fn transform(db: &str, from: &str) -> String {
    let with = bindings(db);
    let row = row(&with);
    super::insert(db, &with, &row, from)
}

/// Every binding the tic holds, in the order the engine computes them.
///
/// `P_Ticker` runs the players, the thinkers and the specials and then
/// bumps `leveltime`; `G_Ticker` runs the status bar, the heads-up display
/// and the menu after it. The melt comes last because it draws its numbers
/// between the tic and the frame that follows it.
fn bindings(db: &str) -> Vec<(String, String)> {
    let mut tic = Tic::new(previous(db));
    tic.stage(super::constants(db));
    let command = game::command(&tic.state, db);
    tic.stage(command);
    let special = game::special_buttons(&tic.state);
    tic.stage(special);
    let specials = spec::update_specials(&tic.state, db);
    tic.stage_when(game::RUNNING, specials);
    tic.stage_when(game::RUNNING, leveltime());
    let tickers = hud::tickers(&tic.state);
    tic.stage(tickers);
    tic.stage(melt());
    tic.bindings
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

/// Each state column named after the binding that holds it: what a stage
/// computed, or the previous tic's value.
fn row(with: &[(String, String)]) -> Vec<(&'static str, String)> {
    state_columns()
        .into_iter()
        .map(|name| {
            if name == "tic" {
                return (name, "tic".to_owned());
            }
            let now = format!("now_{name}");
            let held = with.iter().any(|(binding, _)| *binding == now);
            (name, if held { now } else { format!("prev_{name}") })
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
        assert!(sql.ends_with("WHERE tic > 0"));
        assert!(sql.contains("joinGet('nat.native_state', 'leveltime', toUInt32(tic - 1))"));
    }

    #[test]
    fn a_step_runs_the_same_transform_over_one_row() {
        let step = step_statement("nat", 7, source::DEMO, 0, (0, 0));
        let resident = resident_statement("nat");
        let head = |sql: &str| sql.split("\nFROM\n").next().unwrap().to_owned();
        assert_eq!(head(&step.sql), head(&resident));
        assert!(step.sql.contains("toUInt32(7) AS tic"));
    }

    #[test]
    fn every_column_the_tic_does_not_compute_is_carried_forward() {
        let row = row(&bindings("nat"));
        assert_eq!(row.len(), state_columns().len());
        let named = |column: &str| {
            row.iter()
                .find(|(name, _)| *name == column)
                .map(|(_, expr)| expr.clone())
                .unwrap()
        };
        assert_eq!(named("m_x"), "prev_m_x");
        assert_eq!(named("leveltime"), "now_leveltime");
        assert_eq!(named("st_clock"), "now_st_clock");
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
        let with = bindings("nat");
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
