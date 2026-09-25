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

use support::arms::{Arm, ArmChecks, Arms};
use support::db::Fixture;
use support::floor;
use support::resident::Session;

/// The tic each crossing's seeded row stands at, and how many tics run
/// from it. The run starts after the seed, so the transform reads the
/// crossing out of it the way it reads any other tic.
const TURBO_AT: u32 = 900;
const TURBO_TICS: u32 = 52;
const RAISE_AT: u32 = 1000;
const RAISE_TICS: u32 = 196;

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

/// The rows every tic run from `arm`'s seed left.
async fn crossed_rows(fixture: &Fixture, arm: &Arm, tag: i64) -> Vec<Crossed> {
    arm.rows(
        fixture,
        "native_state",
        &format!(
            "tic, \
             arrayFirstIndex((k, t) -> k = {FLOOR} AND t = {tag}, s_kind, s_tag) AS slot, \
             sec_floorheight[{sector}] AS floorheight, \
             unresolved",
            FLOOR = sector_thinker_kind::FLOOR,
            sector = FLOOR_SECTOR + 1,
        ),
    )
    .await
    .into_iter()
    .filter(|row: &Crossed| row.tic > arm.at)
    .collect()
}

/// Line 187 (`lv_lines`), a WR special 98 tagged 3, and a point five map
/// units to each side of its own diagonal run. `EV_DoFloor`'s turboLower
/// lowers sector 117 to sector 116's own floor, 8 map units past it since
/// that differs from sector 117's own, at `FLOORSPEED * 4`.
fn turbo_lower() -> Arm {
    Arm {
        name: "turbo_lower",
        from: 1,
        overrides: vec![
            put("m_x", "toInt32(-5144576)".to_owned()),
            put("m_y", "toInt32(-135541555)".to_owned()),
            put("m_momx", "toInt32(327680)".to_owned()),
            put("m_momy", "toInt32(26214)".to_owned()),
            put("m_z", format!("toInt32({NEIGHBOR_FLOOR})")),
            put("m_floorz", format!("toInt32({NEIGHBOR_FLOOR})")),
            put("m_ceilingz", format!("toInt32({NEIGHBOR_CEILING})")),
        ],
        at: TURBO_AT,
        inputs: (TURBO_AT + 1..=TURBO_AT + TURBO_TICS)
            .map(|tic| Input::keys(tic, 0, (0, 0)))
            .collect(),
    }
}

/// Sector 117's own floor for the raise, seeded already lowered the way
/// `turboLower` leaves it, so `EV_DoFloor`'s raiseFloor has somewhere to
/// climb back to.
const SEEDED_FLOOR: i32 = NEIGHBOR_FLOOR + (8 << 16);

/// One of lines 486-491 or 907 (`lv_lines`), each a WR special 91 tagged
/// 3, and a point five map units to each side of line 486's own
/// horizontal run. raiseFloor climbs to sector 116's own ceiling, clamped
/// to sector 117's own (`FLOORSPEED`, no clamp needed here since it stands
/// lower).
fn raise_floor() -> Arm {
    Arm {
        name: "raise_floor",
        from: 1,
        overrides: vec![
            put("m_x", "toInt32(-25165824)".to_owned()),
            put("m_y", "toInt32(-131923968)".to_owned()),
            put("m_momx", "toInt32(0)".to_owned()),
            put("m_momy", "toInt32(655360)".to_owned()),
            put("m_z", format!("toInt32({NEIGHBOR_FLOOR})")),
            put("m_floorz", format!("toInt32({NEIGHBOR_FLOOR})")),
            put("m_ceilingz", format!("toInt32({NEIGHBOR_CEILING})")),
            (
                "sec_floorheight",
                format!(
                    "arrayMap((v, i) -> if(i = {}, toInt32({SEEDED_FLOOR}), v), \
                     p.sec_floorheight, arrayEnumerate(p.sec_floorheight))",
                    FLOOR_SECTOR + 1
                ),
            ),
        ],
        at: RAISE_AT,
        inputs: (RAISE_AT + 1..=RAISE_AT + RAISE_TICS)
            .map(|tic| Input::keys(tic, 0, (0, 0)))
            .collect(),
    }
}

/// A crossing of either line spawns the floor `EV_DoFloor` spawns, and
/// `T_MoveFloor` runs it to its destination and off the list.
#[tokio::test]
async fn a_crossing_of_a_tagged_line_spawns_the_floor_ev_do_floor_spawns() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_floor").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let arms = Arms::new(vec![Input::demo(1)], vec![turbo_lower(), raise_floor()]);
    let mut session = Session::open(&fixture, false).await;
    arms.drive(&fixture, &mut session).await;
    session.close().await;

    let mut checks = arms.checks();
    let rows = crossed_rows(&fixture, arms.arm("turbo_lower"), FLOOR_TAG).await;
    let dest = NEIGHBOR_FLOOR + (8 << 16);
    the_floor_runs(
        checks.arm("turbo_lower"),
        &rows,
        TURBO_TICS,
        floor::Floor::turbo_lower(FLOOR_SECTOR_HIGH, dest),
        dest,
    );
    let rows = crossed_rows(&fixture, arms.arm("raise_floor"), FLOOR_TAG).await;
    the_floor_runs(
        checks.arm("raise_floor"),
        &rows,
        RAISE_TICS,
        floor::Floor::raise(SEEDED_FLOOR, FLOOR_SECTOR_HIGH),
        FLOOR_SECTOR_HIGH,
    );
    fixture.finish().await;
    checks.finish();
}

/// Checks a crossing's rows against `want`, the reader's own floor, which
/// stops at `dest`.
fn the_floor_runs(
    mut c: ArmChecks<'_>,
    rows: &[Crossed],
    tics: u32,
    mut want: floor::Floor,
    dest: i32,
) {
    c.eq(rows.len(), tics as usize, "every tic ran");
    for row in rows {
        c.eq(
            crossing_unresolved(row.unresolved),
            0,
            format!("tic {} left a crossing unresolved", row.tic),
        );
    }
    let Some(spawned) = rows.iter().find(|row| row.slot != 0) else {
        c.check(false, "no tic after the crossing spawns the floor");
        return;
    };

    let first = want.tic();
    c.eq(
        spawned.floorheight,
        first.floorheight,
        "the spawn tic's own first move",
    );

    let mut done_tic = None;
    for row in rows.iter().filter(|row| row.tic > spawned.tic) {
        if want.done {
            c.eq(
                row.slot,
                0,
                format!("tic {} keeps the floor off the list", row.tic),
            );
            c.eq(row.floorheight, dest, format!("tic {}", row.tic));
            continue;
        }
        let step = want.tic();
        if want.done {
            done_tic = Some(row.tic);
        } else {
            c.check(
                row.slot != 0,
                format!("tic {} still carries the floor", row.tic),
            );
        }
        c.eq(
            row.floorheight,
            step.floorheight,
            format!("tic {}", row.tic),
        );
    }
    c.check(
        done_tic.is_some(),
        "the run reaches the point the floor stops",
    );
}
