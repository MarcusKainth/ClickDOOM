//! A moving platform against a real ClickHouse server.
//!
//! `demo3` reaches its first plat at gametic 603, past the point where the
//! monsters this lane has not written change where the player goes, so the
//! reference trace cannot arbitrate one yet. One test seeds a plat into a
//! state row directly and runs the tic transform over it; another seeds a
//! crossing of the line that spawns one instead. Both check the floor the
//! run leaves against `native/tests/support/plat.rs`, a reader written from
//! `p_plats.c` and `p_floor.c`.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::sql::sim::tick::Input;
use clickdoom_native::{load, sql, wad::Wad};
use clickdoom_spec::native_state::sector_thinker_kind;
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::plat;
use support::seed;

/// The tic the seeded row stands at. The run starts after it, so the
/// transform reads the plat out of it the way it reads any other tic.
const SEED_TIC: u32 = 900;

/// How many tics the plat runs for. Long enough to reach the bottom, wait
/// and start back up.
const TICS: u32 = 130;

/// The sector the plat drives. Nothing stands in it, so the clip finds
/// nothing to move and the floor is free to run.
const SECTOR: usize = 5;

/// How far below its floor the plat runs, in map units.
const DROP: i32 = 64 << 16;

#[derive(Row, Deserialize)]
struct Ran {
    tic: u32,
    floor: i32,
    status: i32,
    count: i32,
    thinkers: u64,
    unresolved: u64,
}

#[tokio::test]
async fn a_seeded_plat_runs_the_way_the_engine_runs_it() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_plat").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, 1));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let floor: i32 = fixture
        .scalar(&format!(
            "SELECT sec_floorheight[{}] FROM {db}.native_state WHERE tic = 1",
            SECTOR + 1
        ))
        .await;
    let seed: Vec<sql::Statement> = plat::seed(&db, SEED_TIC, SECTOR, floor, floor - DROP)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    if let Err(error) = fixture.execute(&seed).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let run = sim::tick::demo_statement(&db, SEED_TIC + 1, SEED_TIC + TICS);
    if let Err(error) = fixture.execute(&[run]).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Ran> = fixture
        .rows(&format!(
            "SELECT tic, sec_floorheight[{}] AS floor, s_status[17] AS status, \
             s_count[17] AS count, toUInt64(length(s_kind)) AS thinkers, unresolved \
             FROM {db}.native_state WHERE tic > {SEED_TIC} ORDER BY tic",
            SECTOR + 1
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), TICS as usize, "every tic ran");
    let mut want = plat::Plat::down_wait_up_stay(floor, floor - DROP);
    for row in &rows {
        assert_eq!(row.unresolved, 0, "tic {} was carried through", row.tic);
        let step = want.tic();
        assert_eq!(
            (row.floor, row.status, row.count),
            (step.floorheight, step.status, step.count),
            "tic {}",
            row.tic
        );
        // The plat only leaves the list once it has reached the top again,
        // which is past the end of this run.
        assert_eq!(row.thinkers, 17, "tic {} keeps the plat", row.tic);
    }
    assert!(
        want.reached_bottom && want.waited,
        "the run has to cover the way down and the wait"
    );
}

/// Line 396 (`lv_lines`), a WR special 88 tagged 1, and a point five map
/// units to each side of its own midpoint (`lv_vertexes` gives the line's
/// ends). The player is put at the first with enough momentum to land on
/// the second, the way a normal walk across it would.
const CROSS_OLD_X: i32 = 87_220_270;
const CROSS_OLD_Y: i32 = -40_837_308;
const CROSS_MOMX: i32 = 147_364;
const CROSS_MOMY: i32 = -638_600;
/// Sector 26, one of line 396's own two sides.
const CROSS_FLOORZ: i32 = 1_048_576;
const CROSS_CEILINGZ: i32 = 13_631_488;

/// Sector 23, tagged 1: what `P_FindSectorFromLineTag` finds for line
/// 396's crossing. Its own floor is the plat's high; the lower of its two
/// two-sided neighbors' (sector 22's, over sector 27's, which stands at
/// the same height as sector 23's own) is `P_FindLowestFloorSurrounding`'s
/// answer and the plat's low.
const CROSSED_SECTOR: usize = 23;
const CROSSED_HIGH: i32 = 9_437_184;
const CROSSED_LOW: i32 = 2_097_152;

/// Tics enough to reach the bottom, wait and start back up.
const CROSS_TICS: u32 = 60;

#[derive(Row, Deserialize)]
struct Crossed {
    tic: u32,
    slot: u32,
    floor: i32,
    status: i32,
    count: i32,
    unresolved: u64,
}

/// A crossing of a WR special 88 line tagged 1 spawns the plat `EV_DoPlat`
/// spawns, and `T_PlatRaise` runs it the same way `a_seeded_plat_runs_the_
/// way_the_engine_runs_it` already checks. The player is seeded across the
/// line rather than run there through `demo3`: gametic 603 is `demo3`'s own
/// first crossing of it, but the monsters this lane has written diverge
/// from the reference trace well before that tic, so a run from tic 1
/// reaches a different game than the one gametic 603 names.
#[tokio::test]
async fn a_crossing_of_the_tagged_line_spawns_the_plat_ev_do_plat_spawns() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_plat_cross").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, 1));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    fn put(column: &'static str, value: String) -> (&'static str, String) {
        (
            column,
            format!(
                "arrayMap((v, k) -> if(k = p.p_mo, {value}, v), \
                 p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    }
    let overrides = [
        put("m_x", format!("toInt32({CROSS_OLD_X})")),
        put("m_y", format!("toInt32({CROSS_OLD_Y})")),
        put("m_momx", format!("toInt32({CROSS_MOMX})")),
        put("m_momy", format!("toInt32({CROSS_MOMY})")),
        put("m_z", format!("toInt32({CROSS_FLOORZ})")),
        put("m_floorz", format!("toInt32({CROSS_FLOORZ})")),
        put("m_ceilingz", format!("toInt32({CROSS_CEILINGZ})")),
    ];
    let mut statements: Vec<sql::Statement> = seed::row(&db, SEED_TIC, 1, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    let inputs: Vec<Input> = (SEED_TIC + 1..=SEED_TIC + CROSS_TICS)
        .map(|tic| Input::keys(tic, 0, (0, 0)))
        .collect();
    statements.push(sim::tick::run_statement(&db, &inputs));
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Crossed> = fixture
        .rows(&format!(
            "SELECT tic, \
             arrayFirstIndex((k, t) -> k = {PLAT} AND t = 1, s_kind, s_tag) AS slot, \
             sec_floorheight[{sector}] AS floor, \
             if(slot = 0, -1, s_status[slot]) AS status, \
             if(slot = 0, -1, s_count[slot]) AS count, \
             unresolved \
             FROM {db}.native_state WHERE tic > {SEED_TIC} ORDER BY tic",
            PLAT = sector_thinker_kind::PLAT,
            sector = CROSSED_SECTOR + 1,
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), CROSS_TICS as usize, "every tic ran");
    // Every other monster the level carries runs too, over 60 tics with no
    // player input to distract them, and one going into a state this lane
    // has not written is a gap this test does not own. What this test owns
    // is the crossing: `cross_plats` runs it, so `PX_CROSSED` and
    // `TX_CROSSED` (a crossing nothing ran) never fire.
    let crossed_bits = sim::unresolved::PX_CROSSED | sim::unresolved::TX_CROSSED;
    for row in &rows {
        assert_eq!(
            row.unresolved & crossed_bits,
            0,
            "tic {} left a crossing unresolved",
            row.tic
        );
    }
    let spawned = rows
        .iter()
        .find(|row| row.slot != 0)
        .unwrap_or_else(|| panic!("no tic after the crossing spawns the plat"));

    // `T_PlatRaise` runs the same tic `EV_DoPlat` spawns the plat, the way
    // a door opens the same tic a use press makes it: the spawn tic's own
    // row is already one step down from the sector's own floor.
    let mut want = plat::Plat::down_wait_up_stay(CROSSED_HIGH, CROSSED_LOW);
    let first = want.tic();
    assert_eq!(
        (spawned.floor, spawned.status, spawned.count),
        (first.floorheight, first.status, first.count),
        "the spawn tic's own first move"
    );
    for row in rows.iter().filter(|row| row.tic > spawned.tic) {
        let step = want.tic();
        assert_eq!(
            (row.floor, row.status, row.count),
            (step.floorheight, step.status, step.count),
            "tic {}",
            row.tic
        );
    }
    assert!(want.reached_bottom, "the run reaches the bottom");
}

#[derive(Row, Deserialize)]
struct CrossedOneShot {
    tic: u32,
    slot: u32,
    floor: i32,
    status: i32,
    count: i32,
    special: i16,
    unresolved: u64,
}

/// The same crossing of line 396 as the WR test above, with the line's own
/// special set to 10 (`downWaitUpStay`, W1) rather than the map's own 88.
/// `EV_DoPlat` spawns the same plat either way; what only a W1 line does is
/// clear its own special once it spawns one, and a dispatch filter reading
/// `line_special` by its own bare name resolves to whatever that same
/// clearing already left the line at, so the crossing that clears its own
/// trigger never finds itself in the list dispatched from.
#[tokio::test]
async fn a_crossing_of_a_one_shot_line_spawns_the_plat_and_clears_the_line() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_plat_oneshot").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, 1));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    fn put(column: &'static str, value: String) -> (&'static str, String) {
        (
            column,
            format!(
                "arrayMap((v, k) -> if(k = p.p_mo, {value}, v), \
                 p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    }
    let overrides = [
        put("m_x", format!("toInt32({CROSS_OLD_X})")),
        put("m_y", format!("toInt32({CROSS_OLD_Y})")),
        put("m_momx", format!("toInt32({CROSS_MOMX})")),
        put("m_momy", format!("toInt32({CROSS_MOMY})")),
        put("m_z", format!("toInt32({CROSS_FLOORZ})")),
        put("m_floorz", format!("toInt32({CROSS_FLOORZ})")),
        put("m_ceilingz", format!("toInt32({CROSS_CEILINGZ})")),
        (
            "line_special",
            "arrayMap((v, i) -> toInt16(if(i = 397, 10, v)), \
             p.line_special, arrayEnumerate(p.line_special))"
                .to_owned(),
        ),
    ];
    let mut statements: Vec<sql::Statement> = seed::row(&db, SEED_TIC, 1, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    let inputs: Vec<Input> = (SEED_TIC + 1..=SEED_TIC + CROSS_TICS)
        .map(|tic| Input::keys(tic, 0, (0, 0)))
        .collect();
    statements.push(sim::tick::run_statement(&db, &inputs));
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<CrossedOneShot> = fixture
        .rows(&format!(
            "SELECT tic, \
             arrayFirstIndex((k, t) -> k = {PLAT} AND t = 1, s_kind, s_tag) AS slot, \
             sec_floorheight[{sector}] AS floor, \
             if(slot = 0, -1, s_status[slot]) AS status, \
             if(slot = 0, -1, s_count[slot]) AS count, \
             line_special[397] AS special, \
             unresolved \
             FROM {db}.native_state WHERE tic > {SEED_TIC} ORDER BY tic",
            PLAT = sector_thinker_kind::PLAT,
            sector = CROSSED_SECTOR + 1,
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), CROSS_TICS as usize, "every tic ran");
    let crossed_bits = sim::unresolved::PX_CROSSED | sim::unresolved::TX_CROSSED;
    for row in &rows {
        assert_eq!(
            row.unresolved & crossed_bits,
            0,
            "tic {} left a crossing unresolved",
            row.tic
        );
    }
    let spawned = rows
        .iter()
        .find(|row| row.slot != 0)
        .unwrap_or_else(|| panic!("no tic after the crossing spawns the plat"));
    assert_eq!(
        spawned.special, 0,
        "the spawn tic clears the line's own one-shot special"
    );

    let mut want = plat::Plat::down_wait_up_stay(CROSSED_HIGH, CROSSED_LOW);
    let first = want.tic();
    assert_eq!(
        (spawned.floor, spawned.status, spawned.count),
        (first.floorheight, first.status, first.count),
        "the spawn tic's own first move"
    );
    for row in rows.iter().filter(|row| row.tic > spawned.tic) {
        let step = want.tic();
        assert_eq!(
            (row.floor, row.status, row.count),
            (step.floorheight, step.status, step.count),
            "tic {}",
            row.tic
        );
        assert_eq!(row.special, 0, "tic {}: the line stays cleared", row.tic);
    }
    assert!(want.reached_bottom, "the run reaches the bottom");
}

/// `p_spec.h`: how long a switch stays pressed.
const BUTTONTIME: i32 = 35;

/// The side the seeded button belongs to, and the picture it puts back.
const BUTTON_LINE: usize = 40;
const BUTTON_TEXTURE: i32 = 3;

#[derive(Row, Deserialize)]
struct Pressed {
    tic: u32,
    timer: i32,
    line: i32,
    texture: i16,
}

/// A switch that was pressed puts its old picture back when the timer runs
/// out, and its slot is freed.
///
/// Nothing presses a switch yet, so the button is seeded the way the plat
/// is. What is checked is `P_UpdateSpecials`' half: the timer counting
/// down, the picture going back on the tic it reaches zero, and the slot
/// emptying.
#[tokio::test]
async fn a_seeded_button_puts_its_picture_back_when_it_runs_out() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_button").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, 1));
    fn put(column: &'static str, value: String) -> (&'static str, String) {
        (
            column,
            format!(
                "arrayMap((v, i) -> if(i = 1, {value}, v), p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    }
    let seeded = support::seed::row(
        &db,
        SEED_TIC,
        1,
        &[
            put("btn_timer", format!("toInt32({BUTTONTIME})")),
            put("btn_line", format!("toInt32({BUTTON_LINE})")),
            put("btn_where", "toInt32(0)".to_owned()),
            put("btn_texture", format!("toInt32({BUTTON_TEXTURE})")),
        ],
    );
    plan.extend(seeded.into_iter().map(sql::Statement::sql));
    plan.push(sim::tick::demo_statement(
        &db,
        SEED_TIC + 1,
        SEED_TIC + BUTTONTIME as u32 + 2,
    ));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Pressed> = fixture
        .rows(&format!(
            "SELECT tic, btn_timer[1] AS timer, btn_line[1] AS line, \
             side_toptexture[1 + line_side0_at] AS texture \
             FROM {db}.native_state \
             CROSS JOIN (SELECT any(side0) AS line_side0_at FROM {db}.lv_lines \
             WHERE id = {BUTTON_LINE}) AS side \
             WHERE tic > {SEED_TIC} ORDER BY tic"
        ))
        .await;
    fixture.finish().await;

    let ends = SEED_TIC + BUTTONTIME as u32;
    for row in &rows {
        let left = ends as i32 - row.tic as i32;
        assert_eq!(row.timer, left.max(0), "the timer at tic {}", row.tic);
        if row.tic < ends {
            assert_eq!(
                row.line, BUTTON_LINE as i32,
                "the slot holds at {}",
                row.tic
            );
        } else {
            assert_eq!(row.line, 0, "the slot is freed at tic {}", row.tic);
            assert_eq!(
                row.texture, BUTTON_TEXTURE as i16,
                "the picture is back at tic {}",
                row.tic
            );
        }
    }
    assert!(rows.iter().any(|r| r.tic > ends), "the run passes the end");
}
