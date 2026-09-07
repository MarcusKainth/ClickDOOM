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

/// The columns the first statement's session streams, in wire order. `pad`
/// carries the padding row the transport writes behind the statement text.
pub const INPUT_SCHEMA: &str =
    "tic UInt32, source UInt8, keys UInt32, mouse_dx Int16, mouse_dy Int16, pad String";

/// The columns the second statement's session streams. It reads everything
/// else back out of [`STAGE_TABLE`], the row the first statement just left
/// there, so a tic number is all it needs.
pub const STAGE2_INPUT_SCHEMA: &str = "tic UInt32, pad String";

/// The table between the two statements: `native_state`'s own columns,
/// keyed by `tic` the same way, plus [`STAGE_EXTRA_COLUMNS`], the scratch
/// bindings a bare alias carries across the boundary rather than a
/// `native_state` column read through `state`.
pub const STAGE_TABLE: &str = "native_stage";

/// `name, type` for each column past the contract that [`STAGE_TABLE`]
/// carries. `cross_dispatch`'s own crossing inputs: the player's own
/// crossed line and the moved things' own, neither a `native_state`
/// column, both read back under these same bare names rather than through
/// `state`.
pub const STAGE_EXTRA_COLUMNS: [(&str, &str); 2] = [
    ("px_crossed_line", "Int64"),
    ("tx_crossed_line", "Array(Int64)"),
];

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

/// The tic's own two resident statements, paired: the second's own input
/// depends on rows only the first writes, so nothing outside this crate
/// opens one without the other.
pub fn resident_statements(db: &str) -> (String, String) {
    (resident_statement_stage1(db), resident_statement_stage2(db))
}

/// The first statement a session opens: one tic command in, one
/// [`STAGE_TABLE`] row out, through the player and the thinkers.
///
/// The padding row the transport writes ahead of the first real row
/// carries tic 0, which the filter drops.
pub(crate) fn resident_statement_stage1(db: &str) -> String {
    transform_stage1(db, &format!("input('{INPUT_SCHEMA}')\nWHERE tic > 0"))
}

/// The second statement a session opens: one tic number in, the
/// [`STAGE_TABLE`] row the first statement left read back, one
/// `native_state` row out, through the specials and `G_Ticker`.
pub(crate) fn resident_statement_stage2(db: &str) -> String {
    transform_stage2(
        db,
        &format!("input('{STAGE2_INPUT_SCHEMA}')\nWHERE tic > 0"),
    )
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

const BATCH_SETTINGS: &str = "\nSETTINGS max_block_size = 1, max_insert_block_size = 1, \
     min_insert_block_size_rows = 1, min_insert_block_size_bytes = 1, \
     max_threads = 1, max_insert_threads = 1";

/// The same two-statement transform over a run of input rows, as two
/// statements a caller issues in order: every tic through [`STAGE_TABLE`],
/// then every tic through `native_state`. Nothing opens the first alone,
/// because the second's own row is not otherwise reachable.
///
/// A row reads the tic before it through `joinGet`, which the `Join`
/// engine makes visible inside the running statement once the rows arrive
/// one block at a time. The settings that make that true are the ones a
/// session sends as URL parameters, and here they travel with the
/// statement. A run is two statements because the transform is analysed
/// per statement and executed per row, exactly as a session's two are.
pub fn run_statement(db: &str, rows: &[Input]) -> [Statement; 2] {
    // The rows come out of `numbers`, which honours the block size the
    // session runs under. A table of literal rows does not, and rows that
    // share a block all read the state from before it.
    let column = |cast: &str, of: &dyn Fn(&Input) -> String| {
        format!(
            "{cast}([{}][1 + number])",
            rows.iter().map(of).collect::<Vec<_>>().join(", ")
        )
    };
    let stage1 = Statement::sql(format!(
        "{}{BATCH_SETTINGS}",
        transform_stage1(
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
    .with(&PARSE_SETTINGS);
    let stage2 = Statement::sql(format!(
        "{}{BATCH_SETTINGS}",
        transform_stage2(
            db,
            &format!(
                "(\n    SELECT\n        {} AS tic\n    FROM numbers({})\n)\nWHERE tic > 0",
                column("toUInt32", &|row: &Input| row.tic.to_string()),
                rows.len()
            ),
        )
    ))
    .with(&PARSE_SETTINGS);
    [stage1, stage2]
}

/// A run of tics the demo lump drives.
pub fn demo_statement(db: &str, first: u32, last: u32) -> [Statement; 2] {
    let rows: Vec<Input> = (first..=last).map(Input::demo).collect();
    run_statement(db, &rows)
}

fn transform_stage1(db: &str, from: &str) -> String {
    let tic = bindings_stage1(db);
    let row = row(&tic.state);
    let extra: Vec<(&str, String)> = STAGE_EXTRA_COLUMNS
        .iter()
        .map(|(name, _)| {
            let (_, expr) = tic
                .bindings
                .iter()
                .find(|(bound, _)| bound == name)
                .unwrap_or_else(|| panic!("no binding named {name} to stage across the boundary"));
            (*name, expr.clone())
        })
        .collect();
    super::insert(
        db,
        STAGE_TABLE,
        &tic.bindings,
        &row,
        &extra,
        from,
        INPUT_COLUMNS,
    )
}

fn transform_stage2(db: &str, from: &str) -> String {
    let tic = bindings_stage2(db);
    let row = row(&tic.state);
    super::insert(db, "native_state", &tic.bindings, &row, &[], from, &["tic"])
}

/// The first statement's own bindings: the player and the thinkers, which
/// is `P_Ticker` up to the point `P_RunThinkers` has run every thinker the
/// tic itself made room for.
fn bindings_stage1(db: &str) -> Tic {
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
    // Runs the tic's own throw's thinker, at the slot the compaction
    // inside `things` appended it to.
    let thrown = mobj::thrown_thinks(&tic.state);
    let running = game::running(&tic.state);
    tic.stage_when(&running, thrown);
    tic
}

/// The second statement's own bindings: the specials, then `G_Ticker`'s
/// status bar, heads-up display and menu, then the melt, which draws its
/// numbers between the tic and the frame that follows it.
///
/// Seeded from [`STAGE_TABLE`] rather than `native_state`, so this reloads
/// the map's own constants fresh the way the whole tic's own first stage
/// already does, since those are not carried in either table.
fn bindings_stage2(db: &str) -> Tic {
    let mut tic = Tic::new(previous_stage2(db));
    tic.stage(super::constants(db));
    let cross_dispatch = specials::cross_dispatch(&tic.state);
    let running = game::running(&tic.state);
    tic.stage_when(&running, cross_dispatch);
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

/// The state row the first statement reads, one `joinGet` per column
/// against `native_state`'s own previous tic.
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

/// The state row the second statement reads: [`STAGE_TABLE`]'s own row for
/// this same tic, the first statement's, plus [`STAGE_EXTRA_COLUMNS`]
/// under their own bare names rather than `prev_`, since `cross_dispatch`
/// reads them that way, not through `state`.
fn previous_stage2(db: &str) -> Vec<(String, String)> {
    let mut bindings: Vec<(String, String)> = state_columns()
        .into_iter()
        .filter(|name| *name != "tic")
        .map(|name| {
            (
                format!("prev_{name}"),
                format!("joinGet('{db}.{STAGE_TABLE}', '{name}', toUInt32(tic))"),
            )
        })
        .collect();
    for (name, _) in STAGE_EXTRA_COLUMNS {
        bindings.push((
            name.to_owned(),
            format!("joinGet('{db}.{STAGE_TABLE}', '{name}', toUInt32(tic))"),
        ));
    }
    bindings
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
    fn the_resident_statements_read_the_session_s_rows() {
        let stage1 = resident_statement_stage1("nat");
        assert!(stage1.contains(&format!("input('{INPUT_SCHEMA}')")));
        assert!(stage1.contains("WHERE tic > 0"));
        assert!(stage1.contains("joinGet('nat.native_state', 'leveltime', toUInt32(tic - 1))"));
        assert!(stage1.contains("INSERT INTO nat.native_stage"));

        let stage2 = resident_statement_stage2("nat");
        assert!(stage2.contains(&format!("input('{STAGE2_INPUT_SCHEMA}')")));
        assert!(stage2.contains("WHERE tic > 0"));
        assert!(stage2.contains("joinGet('nat.native_stage', 'leveltime', toUInt32(tic))"));
        assert!(stage2.contains("INSERT INTO nat.native_state"));
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
    /// `P_XYMovement` returns before the friction for a skull in flight: a
    /// move it cannot make slams it back into its spawn frames, where
    /// taking friction off it would be wrong. A missile already on the
    /// list carries `missile::thinks_fold` for its own move instead, so
    /// this no longer reaches it. E1M7 holds no skull, so nothing on the
    /// demo reaches this and the statement's own text is what says the
    /// refusal is there.
    #[test]
    fn a_flying_skull_leaves_the_tic_unresolved() {
        /// `p_mobj.h`: `MF_SKULLFLY`.
        const REFUSED: i64 = 0x100_0000;
        let sql = resident_statement_stage1("nat");
        assert!(
            sql.contains(&format!("m_flags[k], {REFUSED}) != 0")),
            "the move refuses the flag"
        );
        assert!(sql.contains("tx_unrun = 1"), "and the refusal is read");
    }

    #[test]
    fn each_caller_of_the_move_test_holds_one() {
        let sql = resident_statement_stage1("nat") + &resident_statement_stage2("nat");
        // The player's own step, the general movers' two parts, the
        // chase, the spawn's own half step, the tic's own throw's move,
        // a missile already in flight's own worst-case draw count (read
        // once for the count and once for whether it is unsure), and its
        // own thinker each hold one. `arrayMap(clip ->` is a moving
        // plane's own crush, in the second statement.
        assert_eq!(sql.matches("arrayMap(mv ->").count(), 9);
        assert_eq!(sql.matches("arrayMap(clip ->").count(), 1);
        assert_eq!(sql.matches("arrayFold((move_at, move_step)").count(), 1);
        assert_eq!(sql.matches("arrayFold((cw_at, cw_step)").count(), 1);
    }

    #[test]
    fn a_step_runs_the_same_transform_over_one_row() {
        let [step1, step2] = run_statement("nat", &[Input::demo(7)]);
        let resident1 = resident_statement_stage1("nat");
        let resident2 = resident_statement_stage2("nat");
        let head = |sql: &str| sql.split("\nFROM\n").next().unwrap().to_owned();
        assert_eq!(head(&step1.sql), head(&resident1));
        assert_eq!(head(&step2.sql), head(&resident2));
        assert!(step1.sql.contains("toUInt32([7][1 + number]) AS tic"));
        assert!(step2.sql.contains("toUInt32([7][1 + number]) AS tic"));
    }

    #[test]
    fn every_column_the_tic_does_not_compute_is_carried_forward() {
        let row = row(&bindings_stage2("nat").state);
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
        for with in [
            bindings_stage1("nat").bindings,
            bindings_stage2("nat").bindings,
        ] {
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
        for with in [
            bindings_stage1("nat").bindings,
            bindings_stage2("nat").bindings,
        ] {
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

    /// The scratch bindings a bare alias crosses the boundary with, and
    /// nothing else.
    ///
    /// A tracked binding (one `now_<column>` names) is renamed to this
    /// statement's own `s<n>_<column>`, a name the other statement's own
    /// numbering never produces by coincidence, so only an untracked one
    /// keeps the literal name a Rust source file could also spell in the
    /// other statement's own generator. [`STAGE_EXTRA_COLUMNS`] are the
    /// only untracked names allowed to.
    #[test]
    fn no_bare_alias_but_the_staged_ones_crosses_the_boundary() {
        let stage1 = bindings_stage1("nat").bindings;
        let stage2 = bindings_stage2("nat").bindings;
        let stage2_names: Vec<&str> = stage2.iter().map(|(name, _)| name.as_str()).collect();
        let scratch: Vec<&str> = stage1
            .iter()
            .map(|(name, _)| name.as_str())
            .filter(|name| super::super::column_of(&format!("now_{name}")).is_none())
            .filter(|name| !name.starts_with("prev_"))
            .filter(|name| {
                !STAGE_EXTRA_COLUMNS
                    .iter()
                    .any(|(carried, _)| carried == name)
            })
            // A map constant both statements reload fresh under the same
            // name is not read across the boundary; it is defined again.
            .filter(|name| !stage2_names.contains(name))
            .collect();
        for (name, expr) in &stage2 {
            for bare in &scratch {
                assert!(
                    !mentions(expr, bare),
                    "{name} reads {bare} across the boundary, which \
                     native_stage does not carry"
                );
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
        for tic in [bindings_stage1("lanew"), bindings_stage2("lanew")] {
            check_no_large_binding_is_copied(&tic);
        }
    }

    fn check_no_large_binding_is_copied(tic: &Tic) {
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
