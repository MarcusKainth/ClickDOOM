//! A moving platform against a real ClickHouse server.
//!
//! `demo3` reaches its first plat at gametic 603, past the point where the
//! monsters this lane has not written change where the player goes, so the
//! reference trace cannot arbitrate one yet. This seeds a plat into a state
//! row instead, runs the tic transform over it, and checks the floor it
//! leaves against `native/tests/support/plat.rs`, a reader written from
//! `p_plats.c` and `p_floor.c`.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::plat;

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
    unresolved: u8,
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
