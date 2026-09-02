//! The light thinkers against a real ClickHouse server.
//!
//! Forty tics of `DEMO3` run through the same transform a session opens,
//! and every sector light, every thinker's count and direction and the
//! random index are checked against `native/tests/support/lights.rs`, a
//! reader written from `p_lights.c`.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::lights::{Lights, Thinker};

/// How far the run goes. Long enough for every kind to have fired more
/// than once: the shortest strobe holds five tics and the longest thirty-
/// five.
const TICS: u32 = 40;

#[tokio::test]
async fn the_lights_flash_the_way_the_engine_flashes_them() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_lights").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, TICS));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Tic> = fixture
        .rows(&format!(
            "SELECT tic, prndindex, sec_lightlevel, s_sector, s_kind, s_count, \
             s_direction, s_minlight, s_maxlight, s_mintime, s_maxtime \
             FROM {db}.native_state ORDER BY tic"
        ))
        .await;
    assert_eq!(rows.len() as u32, TICS + 1);

    let rnd = tables::table("rndtable").unwrap().ints("value").unwrap();
    let start = &rows[0];
    assert!(!start.s_kind.is_empty(), "E1M7 has light thinkers");
    let mut lights = Lights {
        lightlevel: start.sec_lightlevel.clone(),
        thinkers: thinkers(start),
        prndindex: u32::from(start.prndindex),
    };
    for row in rows.iter().skip(1) {
        lights.tic(&rnd);
        assert_eq!(
            row.sec_lightlevel, lights.lightlevel,
            "the sector lights at tic {}",
            row.tic
        );
        assert_eq!(
            row.s_count,
            lights.thinkers.iter().map(|t| t.count).collect::<Vec<_>>(),
            "the counts at tic {}",
            row.tic
        );
        assert_eq!(
            row.s_direction,
            lights
                .thinkers
                .iter()
                .map(|t| t.direction)
                .collect::<Vec<_>>(),
            "the directions at tic {}",
            row.tic
        );
        assert_eq!(
            u32::from(row.prndindex),
            lights.prndindex,
            "the random index at tic {}",
            row.tic
        );
    }
    assert!(
        lights.prndindex != u32::from(start.prndindex),
        "the run draws from the random table at least once"
    );
}

#[derive(Row, Deserialize)]
struct Tic {
    tic: u32,
    prndindex: u8,
    sec_lightlevel: Vec<i16>,
    s_sector: Vec<i32>,
    s_kind: Vec<u8>,
    s_count: Vec<i32>,
    s_direction: Vec<i32>,
    s_minlight: Vec<i32>,
    s_maxlight: Vec<i32>,
    s_mintime: Vec<i32>,
    s_maxtime: Vec<i32>,
}

fn thinkers(row: &Tic) -> Vec<Thinker> {
    (0..row.s_kind.len())
        .map(|at| Thinker {
            sector: row.s_sector[at],
            kind: row.s_kind[at],
            count: row.s_count[at],
            direction: row.s_direction[at],
            minlight: row.s_minlight[at],
            maxlight: row.s_maxlight[at],
            mintime: row.s_mintime[at],
            maxtime: row.s_maxtime[at],
        })
        .collect()
}
