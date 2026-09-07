//! Crossing-triggered floors against a real ClickHouse server.
//!
//! Sector 117 on E1M7 is tagged 3: line 187 (special 98, turboLower) and
//! lines 486-491 and 907 (special 91, raiseFloor) all name it, and every
//! one of sector 117's own two-sided neighbors is sector 116, so both
//! kinds' destinations come from that one sector alone. The player is
//! seeded across each line the way `sim_plat_live.rs`'s and
//! `sim_door_live.rs`'s own crossing tests are, since `demo3` does not
//! reach either line. Each floor's own run is checked against
//! `native/tests/support/floor.rs`, a reader written from `p_floor.c`.
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
use support::floor;
use support::seed;

/// The tic the seeded row stands at. The run starts after it, so the
/// transform reads the crossing out of it the way it reads any other tic.
const SEED_TIC: u32 = 900;

/// Sector 117, tagged 3: floor 248 map units, ceiling 320. Sector 116, its
/// only two-sided neighbor (every one of its own lines borders it): floor
/// 48, ceiling 248.
const FLOOR_TAG: i64 = 3;
const FLOOR_SECTOR_HIGH: i32 = 16_252_928;
const NEIGHBOR_FLOOR: i32 = 3_145_728;
const NEIGHBOR_CEILING: i32 = 16_252_928;

/// The player's whole level runs while the floor does, and another
/// monster hitting a gap this test does not own is not evidence against
/// the floor. What this test owns is the crossing.
fn crossing_unresolved(unresolved: u64) -> u64 {
    unresolved
        & (sim::unresolved::PX_CROSSED
            | sim::unresolved::TX_CROSSED
            | sim::unresolved::PX_MULTI_CROSSED
            | sim::unresolved::TX_MULTI_CROSSED)
}

#[derive(Row, Deserialize)]
struct Crossed {
    tic: u32,
    slot: u32,
    floorheight: i32,
    unresolved: u64,
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

/// Sector 117 (0-based), the tagged sector both floors here drive.
const FLOOR_SECTOR: usize = 117;

async fn crossed_rows(fixture: &Fixture, db: &str, tag: i64) -> Vec<Crossed> {
    fixture
        .rows(&format!(
            "SELECT tic, \
             arrayFirstIndex((k, t) -> k = {FLOOR} AND t = {tag}, s_kind, s_tag) AS slot, \
             sec_floorheight[{sector}] AS floorheight, \
             unresolved \
             FROM {db}.native_state WHERE tic > {SEED_TIC} ORDER BY tic",
            FLOOR = sector_thinker_kind::FLOOR,
            sector = FLOOR_SECTOR + 1,
        ))
        .await
}

/// Line 187 (`lv_lines`), a WR special 98 tagged 3, and a point five map
/// units to each side of its own diagonal run. `EV_DoFloor`'s turboLower
/// lowers sector 117 to sector 116's own floor, 8 map units past it since
/// that differs from sector 117's own, at `FLOORSPEED * 4`.
#[tokio::test]
async fn a_crossing_of_the_turbo_lower_line_spawns_the_floor_ev_do_floor_spawns() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_floor_turbo").await;
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
        put("m_x", "toInt32(-5144576)".to_owned()),
        put("m_y", "toInt32(-135541555)".to_owned()),
        put("m_momx", "toInt32(327680)".to_owned()),
        put("m_momy", "toInt32(26214)".to_owned()),
        put("m_z", format!("toInt32({NEIGHBOR_FLOOR})")),
        put("m_floorz", format!("toInt32({NEIGHBOR_FLOOR})")),
        put("m_ceilingz", format!("toInt32({NEIGHBOR_CEILING})")),
    ];
    let seeded: Vec<sql::Statement> = seed::row(&db, SEED_TIC, 1, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    const CROSS_TICS: u32 = 52;
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let inputs: Vec<Input> = (SEED_TIC + 1..=SEED_TIC + CROSS_TICS)
        .map(|tic| Input::keys(tic, 0, (0, 0)))
        .collect();
    support::resident::run(&fixture, &inputs, false).await;

    let rows = crossed_rows(&fixture, &db, FLOOR_TAG).await;
    fixture.finish().await;

    assert_eq!(rows.len(), CROSS_TICS as usize, "every tic ran");
    for row in &rows {
        assert_eq!(
            crossing_unresolved(row.unresolved),
            0,
            "tic {} left a crossing unresolved",
            row.tic
        );
    }
    let spawned = rows
        .iter()
        .find(|row| row.slot != 0)
        .unwrap_or_else(|| panic!("no tic after the crossing spawns the floor"));

    let dest = NEIGHBOR_FLOOR + (8 << 16);
    let mut want = floor::Floor::turbo_lower(FLOOR_SECTOR_HIGH, dest);
    let first = want.tic();
    assert_eq!(
        spawned.floorheight, first.floorheight,
        "the spawn tic's own first move"
    );

    let mut done_tic = None;
    for row in rows.iter().filter(|row| row.tic > spawned.tic) {
        if want.done {
            assert_eq!(row.slot, 0, "tic {} keeps the floor off the list", row.tic);
            assert_eq!(row.floorheight, dest, "tic {}", row.tic);
            continue;
        }
        let step = want.tic();
        if want.done {
            done_tic = Some(row.tic);
        } else {
            assert_ne!(row.slot, 0, "tic {} still carries the floor", row.tic);
        }
        assert_eq!(row.floorheight, step.floorheight, "tic {}", row.tic);
    }
    assert!(
        done_tic.is_some(),
        "the run reaches the point the floor stops"
    );
}

/// One of lines 486-491 or 907 (`lv_lines`), each a WR special 91 tagged
/// 3, and a point five map units to each side of line 486's own
/// horizontal run. Sector 117's own floor is seeded already lowered, the
/// way `turboLower` above leaves it, so `EV_DoFloor`'s raiseFloor has
/// somewhere to climb back to: sector 116's own ceiling, clamped to
/// sector 117's own (`FLOORSPEED`, no clamp needed here since it stands
/// lower).
#[tokio::test]
async fn a_crossing_of_the_raise_floor_line_spawns_the_floor_ev_do_floor_spawns() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_floor_raise").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.extend(sim::tick::demo_statement(&db, 1, 1));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let seeded_floor = NEIGHBOR_FLOOR + (8 << 16);
    let mut overrides = vec![
        put("m_x", "toInt32(-25165824)".to_owned()),
        put("m_y", "toInt32(-131923968)".to_owned()),
        put("m_momx", "toInt32(0)".to_owned()),
        put("m_momy", "toInt32(655360)".to_owned()),
        put("m_z", format!("toInt32({NEIGHBOR_FLOOR})")),
        put("m_floorz", format!("toInt32({NEIGHBOR_FLOOR})")),
        put("m_ceilingz", format!("toInt32({NEIGHBOR_CEILING})")),
    ];
    overrides.push((
        "sec_floorheight",
        format!(
            "arrayMap((v, i) -> if(i = {}, toInt32({seeded_floor}), v), \
             p.sec_floorheight, arrayEnumerate(p.sec_floorheight))",
            FLOOR_SECTOR + 1
        ),
    ));
    let seeded: Vec<sql::Statement> = seed::row(&db, SEED_TIC, 1, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    const CROSS_TICS: u32 = 196;
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let inputs: Vec<Input> = (SEED_TIC + 1..=SEED_TIC + CROSS_TICS)
        .map(|tic| Input::keys(tic, 0, (0, 0)))
        .collect();
    support::resident::run(&fixture, &inputs, false).await;

    let rows = crossed_rows(&fixture, &db, FLOOR_TAG).await;
    fixture.finish().await;

    assert_eq!(rows.len(), CROSS_TICS as usize, "every tic ran");
    for row in &rows {
        assert_eq!(
            crossing_unresolved(row.unresolved),
            0,
            "tic {} left a crossing unresolved",
            row.tic
        );
    }
    let spawned = rows
        .iter()
        .find(|row| row.slot != 0)
        .unwrap_or_else(|| panic!("no tic after the crossing spawns the floor"));

    let mut want = floor::Floor::raise(seeded_floor, FLOOR_SECTOR_HIGH);
    let first = want.tic();
    assert_eq!(
        spawned.floorheight, first.floorheight,
        "the spawn tic's own first move"
    );

    let mut done_tic = None;
    for row in rows.iter().filter(|row| row.tic > spawned.tic) {
        if want.done {
            assert_eq!(row.slot, 0, "tic {} keeps the floor off the list", row.tic);
            assert_eq!(row.floorheight, FLOOR_SECTOR_HIGH, "tic {}", row.tic);
            continue;
        }
        let step = want.tic();
        if want.done {
            done_tic = Some(row.tic);
        } else {
            assert_ne!(row.slot, 0, "tic {} still carries the floor", row.tic);
        }
        assert_eq!(row.floorheight, step.floorheight, "tic {}", row.tic);
    }
    assert!(
        done_tic.is_some(),
        "the run reaches the point the floor stops"
    );
}
