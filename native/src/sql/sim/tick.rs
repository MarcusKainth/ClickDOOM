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
/// crossed line and the moved things' own. `mt_light_index` is where the
/// sector thinkers start reading the random table, which the mobj stage
/// works out from arrays only it holds. None of the three is a
/// `native_state` column, and all three are read back under these same
/// bare names rather than through `state`.
pub const STAGE_EXTRA_COLUMNS: [(&str, &str); 3] = [
    ("px_crossed_line", "Int64"),
    ("tx_crossed_line", "Array(Int64)"),
    ("mt_light_index", "UInt8"),
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
/// The first statement over `rows`, cut short at `cut`. Shared by
/// [`run_statement`] and the cut statements the bench module builds, so a
/// measured statement differs from the shipped one only by the cut.
fn stage1_over(db: &str, rows: &[Input], cut: Option<&'static str>) -> Statement {
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
        "{}{BATCH_SETTINGS}",
        transform_stage1_cut(
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
            cut,
        )
    ))
    .with(&PARSE_SETTINGS)
}

/// Building the first statement cut short, for measuring what each of its
/// stages costs. Behind the test feature, so the release surface carries
/// none of it.
///
/// A cut leaves out everything from that point on, and the columns the cut
/// stages would have written fall back to the tic before. Every cut is
/// therefore a statement writing the same row as the shipped one, over the
/// same input, differing only in what it computes.
#[cfg(feature = "clickhouse-tests")]
pub mod bench {
    use super::{Input, Statement};

    /// Where the first statement can be cut short, in the order it
    /// computes them. The cost of a stage is the difference between the
    /// cut that ends before it and the cut after it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Cut {
        /// After the tic command, before the player thinks.
        Command,
        /// After the player, before the sector thinkers' own draw count.
        Think,
        /// After that count, before the thing thinkers.
        Lights,
        /// Inside the thing thinkers, before each named piece.
        Moves,
        Falls,
        Look,
        Sight,
        Chase,
        Draws,
        Crowd,
        Strikes,
        Missile,
        Compact,
        Project,
        /// After every thing thinker, before the tic's own throw runs.
        Thinkers,
    }

    impl Cut {
        /// Every cut, in the order the statement computes them.
        pub const ALL: [Cut; 15] = [
            Cut::Command,
            Cut::Think,
            Cut::Lights,
            Cut::Moves,
            Cut::Falls,
            Cut::Look,
            Cut::Sight,
            Cut::Chase,
            Cut::Draws,
            Cut::Crowd,
            Cut::Strikes,
            Cut::Missile,
            Cut::Compact,
            Cut::Project,
            Cut::Thinkers,
        ];

        /// The name the generator knows this cut by.
        pub fn name(self) -> &'static str {
            match self {
                Cut::Command => "command",
                Cut::Think => "think",
                Cut::Lights => "lights",
                Cut::Moves => "moves",
                Cut::Falls => "falls",
                Cut::Look => "look",
                Cut::Sight => "sight",
                Cut::Chase => "chase",
                Cut::Draws => "draws",
                Cut::Crowd => "crowd",
                Cut::Strikes => "strikes",
                Cut::Missile => "missile",
                Cut::Compact => "compact",
                Cut::Project => "project",
                Cut::Thinkers => "thinkers",
            }
        }
    }

    /// The first statement over `rows`, cut short at `cut`, in the shape
    /// [`super::run_statement`] builds: driven from `numbers()` and
    /// reading the tic before out of `native_state`. `None` gives the
    /// statement as it ships.
    ///
    /// Reading the tic before out of the table is what keeps a cut
    /// comparable: the caller puts a known row there, so every cut runs
    /// the same tic over the same world instead of each one drifting into
    /// a world of its own.
    pub fn stage1(db: &str, cut: Option<Cut>, rows: &[Input]) -> Statement {
        super::stage1_over(db, rows, cut.map(Cut::name))
    }
}

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
    let stage1 = stage1_over(db, rows, None);
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
    transform_stage1_cut(db, from, None)
}

/// [`transform_stage1`], cut short at `cut`. `None` is the statement as it
/// ships, which is the only one a release build asks for.
fn transform_stage1_cut(db: &str, from: &str, cut: Option<&'static str>) -> String {
    let tic = bindings_stage1(db, cut);
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
///
/// `cut` leaves out everything from that point on. A column the cut stage
/// would have written falls back to the tic before through `prev_`, so a
/// cut statement writes the same row and differs only in what it computes
/// for it. `None` builds the statement as it ships.
fn bindings_stage1(db: &str, cut: Option<&'static str>) -> Tic {
    let mut tic = Tic::new(previous(db));
    tic.stage(super::constants(db));
    let command = game::command(&tic.state, db);
    tic.stage(command);
    let special = game::special_buttons(&tic.state);
    tic.stage(special);
    if cut == Some("command") {
        tic.stage(uncrossed(true, true));
        return tic;
    }
    let think = player::think(&tic.state);
    let running = game::running(&tic.state);
    tic.stage_when(&running, think);
    if cut == Some("think") {
        tic.stage(uncrossed(true, true));
        return tic;
    }
    // The sector thinkers sit partway up the thinker list, so how many
    // numbers they draw is known before the stage that steps over them.
    tic.stage(lights::draws(&tic.state));
    if cut == Some("lights") {
        tic.stage(uncrossed(true, true));
        return tic;
    }
    let things = match cut.and_then(inside_thinkers) {
        None => mobj::thinkers(&tic.state),
        Some(marker) => cut_before(mobj::thinkers(&tic.state), marker),
    };
    // Which of the boundary's own scratch columns this cut still reaches.
    // Asking the bindings rather than naming the cuts means a stage that
    // moves cannot leave a cut quietly missing one.
    let has = |column: &str| things.iter().any(|(name, _)| name == column);
    let (crossings, light_index) = (has("tx_crossed_line"), has("mt_light_index"));
    let running = game::running(&tic.state);
    tic.stage_when(&running, things);
    if !crossings || !light_index {
        tic.stage(uncrossed(!crossings, !light_index));
    }
    if cut == Some("thinkers") || cut.and_then(inside_thinkers).is_some() {
        return tic;
    }
    // Runs the tic's own throw's thinker, at the slot the compaction
    // inside `things` appended it to.
    let thrown = mobj::thrown_thinks(&tic.state);
    let running = game::running(&tic.state);
    tic.stage_when(&running, thrown);
    tic
}

/// Each cut that stops inside `mobj::thinkers`, and the binding it stops
/// before, in the order that stage computes them.
const THINKER_CUTS: [(&str, &str); 11] = [
    ("moves", "tx_moving"),
    ("falls", "tz_falling"),
    ("look", "mt_cycles"),
    ("sight", "mt_pairs"),
    ("chase", "mc_m_angle"),
    ("draws", "mt_shouts"),
    ("crowd", "mt_entries"),
    ("strikes", "at_asks"),
    ("missile", "mt_missile_asks"),
    ("compact", "mt_gone"),
    ("project", "now_m_x"),
];

/// The binding `cut` stops before, for a cut inside `mobj::thinkers`.
fn inside_thinkers(cut: &'static str) -> Option<&'static str> {
    THINKER_CUTS
        .iter()
        .find(|(name, _)| *name == cut)
        .map(|(_, marker)| *marker)
}

/// `bindings` up to the first one named `marker`.
fn cut_before(bindings: Vec<(String, String)>, marker: &str) -> Vec<(String, String)> {
    let at = bindings
        .iter()
        .position(|(name, _)| name == marker)
        .unwrap_or_else(|| panic!("no binding named {marker} to cut before"));
    bindings[..at].to_vec()
}

/// The [`STAGE_EXTRA_COLUMNS`] a cut stage never wrote: no line crossed,
/// and the sector thinkers reading the random table where the tic left it.
/// Only a cut statement needs them, and a cut that keeps the stage which
/// writes one passes `false` for it.
fn uncrossed(crossings: bool, light_index: bool) -> Vec<(String, String)> {
    let mut stubs = Vec::new();
    if crossings {
        stubs.push(("px_crossed_line".to_owned(), "toInt64(-1)".to_owned()));
        stubs.push((
            "tx_crossed_line".to_owned(),
            "arrayMap(v -> toInt64(-1), prev_m_x)".to_owned(),
        ));
    }
    if light_index {
        stubs.push(("mt_light_index".to_owned(), "prev_prndindex".to_owned()));
    }
    stubs
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
    let thinkers = lights::thinkers(&tic.state, "mt_light_index");
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

    /// The count the mobj stage adds to a slot's base is what the light
    /// fold goes on to draw. The two sit in different statements, so the
    /// tic has to leave the sector thinkers alone between them, or append
    /// only thinkers that draw nothing.
    ///
    /// The player's own use of a line runs ahead of the count, and a
    /// crossing runs behind it in the second statement; both append. A
    /// stage that writes a column is renamed to `s<n>_<column>`, so this
    /// reads the binding names in the order the stages added them, and
    /// follows what the crossing's own append reaches, since the kind it
    /// writes sits in a binding of its own.
    #[test]
    fn nothing_drawing_joins_the_sector_thinkers_between_the_count_and_the_fold() {
        use clickdoom_spec::native_state::sector_thinker_kind as kind;

        let at = |bindings: &[(String, String)], want: &str| -> usize {
            bindings
                .iter()
                .position(|(name, _)| name == want)
                .unwrap_or_else(|| panic!("{want} is bound"))
        };
        let writes = |bindings: &[(String, String)], suffix: &str| -> Vec<(String, String)> {
            bindings
                .iter()
                .filter(|(name, _)| name.ends_with(suffix) && !name.starts_with("prev_"))
                .cloned()
                .collect()
        };

        // Nothing writes either column after the count, so the row the
        // first statement leaves is the one the count was taken over.
        let stage1 = bindings_stage1("nat", None).bindings;
        let count = at(&stage1, "lt_draws");
        for suffix in ["_s_kind", "_s_count"] {
            assert!(
                writes(&stage1[count..], suffix).is_empty(),
                "the first statement writes {suffix} after the count"
            );
            assert!(
                !writes(&stage1[..count], suffix).is_empty(),
                "nothing writes {suffix} at all, so this proves nothing"
            );
        }

        // A crossing appends a door, a plat or a floor ahead of the fold.
        // None of the three reaches the switch that draws, so the count
        // the first statement took still says how many numbers the fold
        // takes.
        let stage2 = bindings_stage2("nat").bindings;
        let fold = at(&stage2, "lights");
        let before = &stage2[..fold];
        let appends = writes(before, "_s_kind");
        assert!(
            !appends.is_empty(),
            "nothing appends, so this proves nothing"
        );
        for (name, expr) in appends {
            let reaches = reached(&expr, before);
            let holds = |written: &str| reaches.iter().any(|text| text.contains(written));
            for drawing in [kind::LIGHT_FLASH, kind::FIRE_FLICKER] {
                assert!(
                    !holds(&format!("toUInt8({drawing})")),
                    "{name} appends a thinker of kind {drawing}, which draws"
                );
            }
            // The kinds a crossing does append are reached this way, so a
            // scan that found none of them would be finding nothing.
            assert!(
                holds(&format!("toUInt8({})", kind::DOOR)),
                "{name} reaches no appended kind at all"
            );
        }
    }

    /// The text of `expr` and of every binding it reaches through the
    /// bindings it names, so a constant a chain of aliases ends at is in
    /// one of the strings this returns. The chain is walked by name
    /// rather than by substitution, which would grow the text by the
    /// product of the chain.
    fn reached(expr: &str, bindings: &[(String, String)]) -> Vec<String> {
        let mut seen: Vec<&str> = Vec::new();
        let mut texts = vec![expr.to_owned()];
        let mut queue = vec![expr.to_owned()];
        while let Some(text) = queue.pop() {
            for (name, of) in bindings {
                if seen.contains(&name.as_str()) || !mentions(&text, name) {
                    continue;
                }
                seen.push(name);
                texts.push(of.clone());
                queue.push(of.clone());
            }
        }
        texts
    }

    /// A `Join` table refuses `ALTER TABLE ... ADD COLUMN`, so
    /// `native_stage`'s column list is kept beside `native_state`'s by
    /// hand. A column added to one and not the other only shows up when a
    /// statement runs, so this reads both lists out of the schema.
    #[test]
    fn the_stage_table_carries_the_contract_and_the_staged_scratch() {
        let columns = |table: &str| -> Vec<(String, String)> {
            crate::sql::schema_columns()
                .into_iter()
                .filter(|(declared, _, _)| *declared == table)
                .map(|(_, column, kind)| (column, kind))
                .collect()
        };
        let extra: Vec<(String, String)> = STAGE_EXTRA_COLUMNS
            .iter()
            .map(|(name, kind)| ((*name).to_owned(), (*kind).to_owned()))
            .collect();
        let want = [columns("native_state"), extra].concat();
        let got = columns(STAGE_TABLE);
        let only_in = |a: &[(String, String)], b: &[(String, String)]| -> Vec<String> {
            a.iter()
                .filter(|column| !b.contains(column))
                .map(|(name, kind)| format!("{name} {kind}"))
                .collect()
        };
        assert_eq!(
            (only_in(&want, &got), only_in(&got, &want)),
            (Vec::new(), Vec::new()),
            "{STAGE_TABLE} is missing the first list and carries the second              beyond native_state and the staged scratch"
        );
        assert_eq!(want, got, "{STAGE_TABLE} declares its columns out of order");
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

    /// Every cut has to build a statement the server could run: its
    /// bindings written before they are read, its parentheses balanced,
    /// and the `native_stage` row still written. A cut that names a
    /// binding the stage no longer computes panics while building, which
    /// is what catches a cut left behind by a stage that moved.
    #[cfg(feature = "clickhouse-tests")]
    #[test]
    fn every_cut_builds_a_statement_the_server_could_run() {
        for cut in bench::Cut::ALL {
            let name = cut.name();
            let with = bindings_stage1("nat", Some(name)).bindings;
            let mut written: Vec<&str> = Vec::new();
            for (binding, expr) in &with {
                for (earlier, _) in &with {
                    if earlier != binding && mentions(expr, earlier) {
                        assert!(
                            written.contains(&earlier.as_str()),
                            "cut {name}: {binding} reads {earlier} before it is written"
                        );
                    }
                }
                written.push(binding);
            }
            for (binding, expr) in &with {
                let depth = expr.chars().fold(0i64, |at, c| match c {
                    '(' => at + 1,
                    ')' => at - 1,
                    _ => at,
                });
                assert_eq!(depth, 0, "cut {name}: {binding} does not balance");
            }
            let sql = bench::stage1("nat", Some(cut), &[Input::demo(1)]).sql;
            assert!(
                sql.contains(&format!("INSERT INTO nat.{STAGE_TABLE}")),
                "cut {name} writes no {STAGE_TABLE} row"
            );
            for (column, _) in STAGE_EXTRA_COLUMNS {
                assert!(sql.contains(column), "cut {name} drops {column}");
            }
        }
    }

    #[test]
    fn a_binding_is_only_read_after_it_is_written() {
        for with in [
            bindings_stage1("nat", None).bindings,
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
            bindings_stage1("nat", None).bindings,
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
        let stage1 = bindings_stage1("nat", None).bindings;
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
        for tic in [bindings_stage1("lanew", None), bindings_stage2("lanew")] {
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
