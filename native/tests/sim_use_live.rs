//! A use press against a real ClickHouse server: a switch that spawns a
//! plat, and a locked door with and without its key.
//!
//! `demo3` reaches all three (tics 612, 1097 and, for the key itself,
//! whatever tic the pickup that carries it falls on), but the monsters
//! this lane has not written change the game well before any of them, so
//! each is seeded here the way the crossing tests are.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::sql::sim::tick::Input;
use clickdoom_native::{load, sql, wad::Wad};
use clickdoom_spec::native_state::{key, sector_thinker_kind};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::door;
use support::plat;
use support::seed;

/// The tic the seeded row stands at. The run starts after it, so the
/// transform reads the press out of it the way it reads any other tic.
const SEED_TIC: u32 = 900;

fn put(column: &'static str, value: String) -> (&'static str, String) {
    (
        column,
        format!(
            "arrayMap((v, k) -> if(k = p.p_mo, {value}, v), \
             p.{column}, arrayEnumerate(p.{column}))"
        ),
    )
}

/// The whole level's own monsters run free over a seeded press too, and
/// one going into a state this lane has not written is a gap none of
/// these tests own. What each one owns is the press: narrowed to the
/// bits `use_special_line` itself can set.
fn use_unresolved(unresolved: u64) -> u64 {
    unresolved
        & (sim::unresolved::MV_UNFINISHED
            | sim::unresolved::PL_ACTION_NEEDED
            | sim::unresolved::USE_UNHANDLED_SPECIAL
            | sim::unresolved::DOOR_OPEN_STUCK)
}

/// Line 397 (`lv_lines`), an SR special 62 tagged 1: the same tag line 396
/// spawns a plat for by crossing, so pressing this one spawns the plat
/// into the same sector 23 the crossing test already names. The line runs
/// north-south, so a point five map units west of its own midpoint, facing
/// east, presses its front side (`P_UseSpecialLine`'s own back-side check
/// only lets special 124 through).
const SWITCH_X: i32 = 90_963_968;
const SWITCH_Y: i32 = -46_137_344;
const SWITCH_ANGLE: u32 = 0;
/// Sector 22, west of line 397.
const SWITCH_FLOORZ: i32 = 2_097_152;
const SWITCH_CEILINGZ: i32 = 8_912_896;

/// Sector 23, tagged 1: `sim_plat_live.rs`'s own crossing test names the
/// same sector and the same high and low.
const SWITCH_SECTOR: usize = 23;
const SWITCH_HIGH: i32 = 9_437_184;
const SWITCH_LOW: i32 = 2_097_152;

/// Line 397's own front side, its top carrying `COMPTALL`: a texture the
/// switch list does not name, the way id Software also left it on this
/// line in the real WAD. The seed row puts `SW1COMP` there instead, so the
/// same press that spawns the plat also exercises `P_ChangeSwitchTexture`.
const SWITCH_SIDE: usize = 499;
/// `switchlist`'s own `SW1COMP`/`SW2COMP` pair, resolved to `tex_textures`.
const SWITCH_TEX_OFF: i16 = 88;
const SWITCH_TEX_ON: i16 = 107;

/// Short of `BUTTONTIME` (35), so the button is still counting down and
/// the picture is still flipped for the whole window this reads.
const SWITCH_TICS: u32 = 30;

#[derive(Row, Deserialize)]
struct Switched {
    tic: u32,
    slot: u32,
    floor: i32,
    status: i32,
    count: i32,
    toptexture: i16,
    btn_line: i32,
    btn_timer: i32,
    unresolved: u64,
}

/// A press of an SR special 62 line spawns the plat `EV_DoPlat` spawns for
/// `downWaitUpStay`, changes the switch's own picture, and starts the
/// button that puts it back. `T_PlatRaise` then runs the plat the same
/// way `sim_plat_live.rs`'s own crossing test checks it.
#[tokio::test]
async fn a_press_of_a_switch_line_spawns_the_plat_and_flips_the_picture() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_use_switch").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.extend(sim::tick::demo_statement(&db, 1, 1));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let overrides = [
        put("m_x", format!("toInt32({SWITCH_X})")),
        put("m_y", format!("toInt32({SWITCH_Y})")),
        put("m_angle", format!("toUInt32({SWITCH_ANGLE})")),
        put("m_z", format!("toInt32({SWITCH_FLOORZ})")),
        put("m_floorz", format!("toInt32({SWITCH_FLOORZ})")),
        put("m_ceilingz", format!("toInt32({SWITCH_CEILINGZ})")),
        (
            "side_toptexture",
            format!(
                "arrayMap((v, i) -> if(i = {}, toInt16({SWITCH_TEX_OFF}), v), \
                 p.side_toptexture, arrayEnumerate(p.side_toptexture))",
                SWITCH_SIDE + 1,
            ),
        ),
    ];
    let seeded: Vec<sql::Statement> = seed::row(&db, SEED_TIC, 1, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let inputs: Vec<Input> = (SEED_TIC + 1..=SEED_TIC + SWITCH_TICS)
        .map(|tic| Input::keys(tic, key::USE, (0, 0)))
        .collect();
    support::resident::run(&fixture, &inputs, false).await;

    let side0: i32 = fixture
        .scalar(&format!(
            "SELECT toInt32(side0) FROM {db}.lv_lines WHERE id = 397"
        ))
        .await;
    let rows: Vec<Switched> = fixture
        .rows(&format!(
            "SELECT tic, \
             arrayFirstIndex((k, t) -> k = {PLAT} AND t = 1, s_kind, s_tag) AS slot, \
             sec_floorheight[{sector}] AS floor, \
             if(slot = 0, -1, s_status[slot]) AS status, \
             if(slot = 0, -1, s_count[slot]) AS count, \
             side_toptexture[{side1}] AS toptexture, \
             if(length(btn_line) = 0, 0, btn_line[1]) AS btn_line, \
             if(length(btn_timer) = 0, 0, btn_timer[1]) AS btn_timer, \
             unresolved \
             FROM {db}.native_state WHERE tic > {SEED_TIC} ORDER BY tic",
            PLAT = sector_thinker_kind::PLAT,
            sector = SWITCH_SECTOR + 1,
            side1 = side0 + 1,
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), SWITCH_TICS as usize, "every tic ran");
    for row in &rows {
        assert_eq!(
            use_unresolved(row.unresolved),
            0,
            "tic {} was carried through",
            row.tic
        );
    }
    let spawned = rows
        .iter()
        .find(|row| row.slot != 0)
        .unwrap_or_else(|| panic!("no tic after the press spawns the plat"));
    assert_eq!(
        spawned.toptexture, SWITCH_TEX_ON,
        "the spawn tic flips the picture to its pair"
    );
    assert_eq!(
        spawned.btn_line, 398,
        "the button names the line, one-based"
    );
    assert_eq!(
        spawned.btn_timer,
        35 - 1,
        "the spawn tic's own first count down, the way a fresh door or \
         plat also takes its first move on the tic that makes it"
    );

    let mut want = plat::Plat::down_wait_up_stay(SWITCH_HIGH, SWITCH_LOW);
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
        assert_eq!(
            row.toptexture, SWITCH_TEX_ON,
            "tic {}: the picture stays flipped while the button counts down",
            row.tic
        );
    }
}

/// Line 698 (`lv_lines`), a manual special 27 (Yellow Door/Locked): sector
/// 41 behind it starts closed (floor and ceiling both 0), and its own
/// two-sided neighbors (24 and 40, both ceiling 4718592) give it a
/// topheight of 4456448, four map units short. The line runs north-south,
/// so a point five map units east of its own midpoint, facing west,
/// presses its front side.
const LOCKED_X: i32 = 45_875_200;
const LOCKED_Y: i32 = -46_137_344;
const LOCKED_ANGLE: u32 = 0x8000_0000;
/// Sector 24, east of line 698.
const LOCKED_FLOORZ: i32 = 0;
const LOCKED_CEILINGZ: i32 = 4_718_592;

const LOCKED_SECTOR: usize = 41;
const LOCKED_TOPHEIGHT: i32 = 4_456_448;
/// `doomdef.h`: `it_yellowcard`, one-based for `p_cards`.
const YELLOW_CARD: usize = 2;

const LOCKED_TICS: u32 = 10;

#[derive(Row, Deserialize)]
struct Locked {
    tic: u32,
    specialdata: u32,
    p_message: u64,
    hu_message: u64,
    unresolved: u64,
}

/// A press of a locked door's own line without the card it needs changes
/// nothing but the message, the way `EV_VerticalDoor`'s own key check
/// leaves it: no thinker, no `DOOR_OPEN_STUCK`, the tic runs to completion.
/// `HU_Ticker` takes `p_message` into the widget the same tic it is set
/// (`hu_stuff.c`: `plr->message = NULL` right after the widget copies it),
/// so the line that survives across tics is `hu_message`, the way
/// `sim_tic_live.rs`'s own message test reads it.
#[tokio::test]
async fn a_press_of_a_locked_door_without_its_key_only_leaves_the_message() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_use_locked").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.extend(sim::tick::demo_statement(&db, 1, 1));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let overrides = [
        put("m_x", format!("toInt32({LOCKED_X})")),
        put("m_y", format!("toInt32({LOCKED_Y})")),
        put("m_angle", format!("toUInt32({LOCKED_ANGLE})")),
        put("m_z", format!("toInt32({LOCKED_FLOORZ})")),
        put("m_floorz", format!("toInt32({LOCKED_FLOORZ})")),
        put("m_ceilingz", format!("toInt32({LOCKED_CEILINGZ})")),
    ];
    let seeded: Vec<sql::Statement> = seed::row(&db, SEED_TIC, 1, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let inputs: Vec<Input> = (SEED_TIC + 1..=SEED_TIC + LOCKED_TICS)
        .map(|tic| Input::keys(tic, key::USE, (0, 0)))
        .collect();
    support::resident::run(&fixture, &inputs, false).await;

    let expect: u64 = fixture
        .scalar("SELECT xxHash64('You need a yellow key to open this door')")
        .await;
    let rows: Vec<Locked> = fixture
        .rows(&format!(
            "SELECT tic, sec_specialdata[{sector}] AS specialdata, \
             p_message, hu_message, \
             unresolved FROM {db}.native_state WHERE tic > {SEED_TIC} ORDER BY tic",
            sector = LOCKED_SECTOR + 1,
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), LOCKED_TICS as usize, "every tic ran");
    for row in &rows {
        assert_eq!(
            use_unresolved(row.unresolved),
            0,
            "tic {} was carried through",
            row.tic
        );
        assert_eq!(row.specialdata, 0, "tic {}: no door spawns", row.tic);
        assert_eq!(row.p_message, 0, "tic {}: the widget empties it", row.tic);
    }
    let pressed = rows
        .iter()
        .find(|row| row.hu_message == expect)
        .unwrap_or_else(|| panic!("no tic leaves the yellow key message"));
    for row in rows.iter().filter(|row| row.tic >= pressed.tic) {
        assert_eq!(
            row.hu_message, expect,
            "tic {}: the message stays up",
            row.tic
        );
    }
}

/// Sector 41's own starting ceiling: closed, the same as its floor.
const LOCKED_START_CEILING: i32 = 0;

#[derive(Row, Deserialize)]
struct Opened {
    tic: u32,
    slot: u32,
    ceiling: i32,
    direction: i32,
    count: i32,
    unresolved: u64,
}

/// The same press, with the player carrying the yellow card: the check
/// passes and `EV_VerticalDoor` opens sector 41 the way any other manual
/// door does, checked against `native/tests/support/door.rs`.
#[tokio::test]
async fn a_press_of_a_locked_door_with_its_key_opens_it() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_use_unlocked").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.extend(sim::tick::demo_statement(&db, 1, 1));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let overrides = [
        put("m_x", format!("toInt32({LOCKED_X})")),
        put("m_y", format!("toInt32({LOCKED_Y})")),
        put("m_angle", format!("toUInt32({LOCKED_ANGLE})")),
        put("m_z", format!("toInt32({LOCKED_FLOORZ})")),
        put("m_floorz", format!("toInt32({LOCKED_FLOORZ})")),
        put("m_ceilingz", format!("toInt32({LOCKED_CEILINGZ})")),
        (
            "p_cards",
            format!(
                "arrayMap((c, i) -> if(i = {YELLOW_CARD}, toUInt8(1), c), \
                 p.p_cards, arrayEnumerate(p.p_cards))"
            ),
        ),
    ];
    let seeded: Vec<sql::Statement> = seed::row(&db, SEED_TIC, 1, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let inputs: Vec<Input> = (SEED_TIC + 1..=SEED_TIC + LOCKED_TICS)
        .map(|tic| Input::keys(tic, key::USE, (0, 0)))
        .collect();
    support::resident::run(&fixture, &inputs, false).await;

    let rows: Vec<Opened> = fixture
        .rows(&format!(
            "SELECT tic, \
             arrayFirstIndex(k -> k = {DOOR}, s_kind) AS slot, \
             sec_ceilingheight[{sector}] AS ceiling, \
             if(slot = 0, -1, s_direction[slot]) AS direction, \
             if(slot = 0, -1, s_count[slot]) AS count, \
             unresolved \
             FROM {db}.native_state WHERE tic > {SEED_TIC} ORDER BY tic",
            DOOR = sector_thinker_kind::DOOR,
            sector = LOCKED_SECTOR + 1,
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), LOCKED_TICS as usize, "every tic ran");
    for row in &rows {
        assert_eq!(
            use_unresolved(row.unresolved),
            0,
            "tic {} was carried through",
            row.tic
        );
    }
    let spawned = rows
        .iter()
        .find(|row| row.slot != 0)
        .unwrap_or_else(|| panic!("no tic after the press spawns the door"));
    let mut want = door::Door::normal(LOCKED_START_CEILING, LOCKED_TOPHEIGHT, LOCKED_FLOORZ);
    let first = want.tic();
    assert_eq!(
        (spawned.ceiling, spawned.direction, spawned.count),
        (first.ceilingheight, first.direction, first.count),
        "the spawn tic's own first move"
    );
}
