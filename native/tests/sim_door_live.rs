//! A crossing-triggered vertical door against a real ClickHouse server.
//!
//! Line 525 on E1M7 is a WR special 90 (`vld_normal`, a retrigger), tagged
//! 5, naming sector 72 alone: a closed door pocket, floor and ceiling both
//! 8 map units, with two two-sided neighbors (sectors 13 and 71) whose
//! lower ceiling (13's, 72 units) is `P_FindLowestCeilingSurrounding`'s own
//! answer. The player is seeded across the line the way
//! `sim_plat_live.rs`'s own crossing test is, since `demo3` never reaches
//! this line either. The door's own run, through the open, the wait at the
//! top and the close, is checked against `native/tests/support/door.rs`, a
//! reader written from `p_doors.c`.
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
use support::door;
use support::seed;

/// The tic the seeded row stands at. The run starts after it, so the
/// transform reads the crossing out of it the way it reads any other tic.
const SEED_TIC: u32 = 900;

/// Line 525 (`lv_lines`), a WR special 90 tagged 5, and a point five map
/// units to each side of its own horizontal run (`lv_vertexes` gives the
/// line's ends; both share one `y`, so the crossing is a step in `y`
/// alone). The player is put at the first with enough momentum to land on
/// the second, the way a normal walk across it would.
const CROSS_OLD_X: i32 = -38_273_024;
const CROSS_OLD_Y: i32 = -4_521_984;
const CROSS_MOMX: i32 = 0;
const CROSS_MOMY: i32 = 655_360;
/// Sector 71, one of line 525's own two sides; sector 3, the other, shares
/// its ceiling but not its floor, and the eight map unit difference is
/// well inside a step the seeded position does not need to be exact about.
const CROSS_FLOORZ: i32 = 524_288;
const CROSS_CEILINGZ: i32 = 16_252_928;

/// Sector 72, tagged 5: what `P_FindSectorFromLineTag` finds for line
/// 525's crossing, a closed door (floor and ceiling both 524288, 8 map
/// units).
const DOOR_TAG: i64 = 5;
const DOOR_START_CEILING: i32 = 524_288;
const DOOR_FLOOR: i32 = 524_288;
/// The lower of sector 72's two two-sided neighbors' ceilings (sector
/// 13's, 72 units; sector 71's stands at 248), four map units short of it,
/// the way `P_FindLowestCeilingSurrounding` and `EV_DoDoor`'s own
/// `- 4*FRACUNIT` leave it.
const DOOR_TOPHEIGHT: i32 = 4_456_448;

/// `VDOORSPEED` covers this distance in 30 tics each way, landing exactly
/// on the destination; `T_MovePlane`'s own "one more step would pass it"
/// test is strict, so `PASTDEST` itself registers one tic later each way.
/// `VDOORWAIT` is 150. Enough tics to run the open, the wait and the
/// close, with a small margin to confirm the thinker is gone.
const CROSS_TICS: u32 = 216;

#[derive(Row, Deserialize)]
struct Crossed {
    tic: u32,
    slot: u32,
    ceiling: i32,
    direction: i32,
    count: i32,
    unresolved: u64,
}

/// A crossing of a WR special 90 line tagged 5 spawns the door `EV_DoDoor`
/// spawns, and `T_VerticalDoor` runs it through the open, the wait at the
/// top and the close, ending with the thinker off the list and the
/// sector's ceiling back at its own floor. Nothing is seeded but the
/// crossing itself: `cross_dispatch`'s own spawn is what this checks.
#[tokio::test]
async fn a_crossing_of_the_tagged_line_spawns_the_door_ev_do_door_spawns() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_door_cross").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.extend(sim::tick::demo_statement(&db, 1, 1));
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
    statements.extend(sim::tick::run_statement(&db, &inputs));
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Crossed> = fixture
        .rows(&format!(
            "SELECT tic, \
             arrayFirstIndex((k, t) -> k = {DOOR} AND t = {DOOR_TAG}, s_kind, s_tag) AS slot, \
             sec_ceilingheight[{sector}] AS ceiling, \
             if(slot = 0, -1, s_direction[slot]) AS direction, \
             if(slot = 0, -1, s_count[slot]) AS count, \
             unresolved \
             FROM {db}.native_state WHERE tic > {SEED_TIC} ORDER BY tic",
            DOOR = sector_thinker_kind::DOOR,
            sector = 72 + 1,
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), CROSS_TICS as usize, "every tic ran");
    let crossed_bits = sim::unresolved::PX_CROSSED
        | sim::unresolved::TX_CROSSED
        | sim::unresolved::PX_MULTI_CROSSED
        | sim::unresolved::TX_MULTI_CROSSED;
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
        .unwrap_or_else(|| panic!("no tic after the crossing spawns the door"));

    // `T_VerticalDoor` runs the same tic `EV_DoDoor` spawns the door, the
    // way a use press's own door does: the spawn tic's own row is already
    // one step up from the sector's own ceiling.
    let mut want = door::Door::normal(DOOR_START_CEILING, DOOR_TOPHEIGHT, DOOR_FLOOR);
    let first = want.tic();
    assert_eq!(
        (spawned.ceiling, spawned.direction, spawned.count),
        (first.ceilingheight, first.direction, first.count),
        "the spawn tic's own first move"
    );

    let mut done_tic = None;
    for row in rows.iter().filter(|row| row.tic > spawned.tic) {
        if want.done {
            assert_eq!(row.slot, 0, "tic {} keeps the door off the list", row.tic);
            assert_eq!(
                row.ceiling, DOOR_FLOOR,
                "tic {}: the door stays closed",
                row.tic
            );
            continue;
        }
        let step = want.tic();
        assert_eq!(row.ceiling, step.ceilingheight, "tic {}", row.tic);
        if want.done {
            assert_eq!(row.slot, 0, "tic {} takes the door off the list", row.tic);
            done_tic = Some(row.tic);
        } else {
            assert_ne!(row.slot, 0, "tic {} still carries the door", row.tic);
            assert_eq!(
                (row.direction, row.count),
                (step.direction, step.count),
                "tic {}",
                row.tic
            );
        }
    }
    assert!(
        want.opened && want.waited && want.closed,
        "the run covers the open, the wait and the close"
    );
    assert!(
        done_tic.is_some(),
        "the run reaches the point the door comes off the list"
    );
}
