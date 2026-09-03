//! `P_NoiseAlert` against a real ClickHouse server.
//!
//! The SQL floods the sectors twice rather than recursing, so it is checked
//! on its own against `native/tests/support/noise.rs`, a reader written
//! from `p_enemy.c` that does recurse, with the "already flooded" test and
//! the sound-block count. The alerts are made in sectors spread through
//! `E1M7` and on both sides of every sound-blocking line, once with the
//! level's own heights and once with some sectors shut, because a shut
//! sector is what takes a line out of the flood.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{maputl, noise};
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;

/// `doomdata.h`
const ML_SOUNDBLOCK: i64 = 64;

/// How many sectors make a noise on their own account, spread evenly
/// through the sector list. The sectors either side of a sound-blocking
/// line are added to them.
const SPREAD: usize = 8;

/// One sector in this many is shut for the second world, which is what
/// takes the lines around it out of the flood.
const SHUT_EVERY: usize = 3;

#[tokio::test]
async fn the_alert_reaches_what_the_engine_reaches() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_noise").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let map = read_map(&fixture).await;
    let sectors = map.floor.len();
    assert!(sectors > 2 * SPREAD, "the level carries enough sectors");
    let mut emitters: Vec<usize> = (0..SPREAD).map(|at| at * sectors / SPREAD).collect();
    for (at, flags) in map.walk.line_flags.iter().enumerate() {
        if flags & ML_SOUNDBLOCK != 0 {
            emitters.push(map.walk.line_front[at]);
            emitters.push(map.walk.line_back[at]);
        }
    }
    emitters.sort_unstable();
    emitters.dedup();

    // The level as it stands, and the same level with one sector in three
    // shut. Between them the fan holds sectors reached across an open
    // line, across a sound-blocking one, and not at all.
    let shut: Vec<i32> = map
        .ceiling
        .iter()
        .enumerate()
        .map(|(at, height)| {
            if at % SHUT_EVERY == 0 {
                map.floor[at]
            } else {
                *height
            }
        })
        .collect();
    let worlds = [
        (
            "the level's own heights",
            "ceilingheight",
            map.ceiling.clone(),
        ),
        (
            "one sector in three shut",
            &format!("if(id % {SHUT_EVERY} = 0, floorheight, ceilingheight)")[..],
            shut,
        ),
    ];

    // The heights travel as bindings rather than as array literals, so the
    // statement holds one copy of each rather than one per alert.
    let mut with: Vec<(String, String)> = maputl::constants(&db);
    with.push(("nz_floor".to_owned(), heights(&db, "floorheight")));
    // One copy of the expression per world, asked about every emitter,
    // because the statement holds an alert's whole tree per copy.
    let sources = format!(
        "[{}]",
        emitters
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut asks: Vec<String> = Vec::new();
    for (at, (_, ceiling, _)) in worlds.iter().enumerate() {
        with.push((format!("nz_ceiling{at}"), heights(&db, ceiling)));
        asks.push(format!(
            "arrayMap(nz_src -> {}, {sources})",
            noise::alert("nz_src", "nz_floor", &format!("nz_ceiling{at}"))
        ));
    }
    let sql = format!(
        "WITH\n{}\nSELECT arrayConcat({}) AS flooded",
        with.into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        asks.join(", ")
    );
    let ours: Flooded = fixture.scalar(&sql).await;
    fixture.finish().await;

    assert_eq!(ours.flooded.len(), worlds.len() * emitters.len());
    let mut counts = [0usize; 3];
    for (at, (name, _, ceiling)) in worlds.iter().enumerate() {
        for (index, emitter) in emitters.iter().enumerate() {
            let want = map.walk.noise_alert(*emitter, &map.floor, ceiling);
            let got = &ours.flooded[at * emitters.len() + index];
            assert_eq!(got, &want, "{name}, from sector {emitter}");
            for level in &want {
                counts[usize::from(*level)] += 1;
            }
        }
    }
    // A flood that answered the same way every time would pass whatever
    // the expression said, and the second level is what the sound-block
    // count is for.
    assert!(
        counts.iter().all(|reached| *reached > 0),
        "unreached {}, near {}, far {}",
        counts[0],
        counts[1],
        counts[2]
    );
}

#[derive(Row, Deserialize)]
struct Flooded {
    flooded: Vec<Vec<u8>>,
}

/// One height per sector, in sector order, the way the tic holds them.
fn heights(db: &str, column: &str) -> String {
    format!(
        "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((id, {column}))))\
         \n     FROM {db}.lv_sectors_static)"
    )
}

/// The level as both sides read it: the walk's own tables and the heights
/// the openings come from.
struct Map {
    walk: support::noise::Map,
    floor: Vec<i32>,
    ceiling: Vec<i32>,
}

#[derive(Row, Deserialize)]
struct Sector {
    floorheight: i32,
    ceilingheight: i32,
    lines: Vec<u32>,
}

#[derive(Row, Deserialize)]
struct Line {
    flags: i16,
    sector0: i32,
    sector1: i32,
}

async fn read_map(fixture: &Fixture) -> Map {
    let db = &fixture.database;
    let sectors: Vec<Sector> = fixture
        .rows(&format!(
            "SELECT floorheight, ceilingheight, lines FROM {db}.lv_sectors_static ORDER BY id"
        ))
        .await;
    let lines: Vec<Line> = fixture
        .rows(&format!(
            "SELECT flags, sector0, sector1 FROM {db}.lv_lines ORDER BY id"
        ))
        .await;
    Map {
        walk: support::noise::Map {
            sector_lines: sectors
                .iter()
                .map(|s| s.lines.iter().map(|l| *l as usize).collect())
                .collect(),
            line_flags: lines.iter().map(|l| i64::from(l.flags)).collect(),
            line_front: lines.iter().map(|l| l.sector0.max(0) as usize).collect(),
            line_back: lines.iter().map(|l| l.sector1.max(0) as usize).collect(),
        },
        floor: sectors.iter().map(|s| s.floorheight).collect(),
        ceiling: sectors.iter().map(|s| s.ceilingheight).collect(),
    }
}
