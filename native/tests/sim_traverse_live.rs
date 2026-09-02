//! The blockmap walk against a real ClickHouse server.
//!
//! `P_PathTraverse` is what the slide, the use line and every hitscan are
//! built on, so it is checked on its own: a fan of traces out of several
//! points of `E1M7`, against `native/tests/support/traverse.rs`, a reader
//! written from `p_maputl.c`.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::maputl;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::traverse::Map;

const FRACUNIT: i64 = 1 << 16;

/// Where the fans start: the player's spawn and three corners of the map
/// the walk has to leave and re-enter.
const FROM: [(i64, i64); 4] = [
    (96 << 16, 528 << 16),
    (352 << 16, 320 << 16),
    (-1000 << 16, -1000 << 16),
    (0, 0),
];

/// How far each trace reaches, in map units. The short one is under the
/// sixteen units the engine changes its side test at.
const REACH: [i64; 3] = [8, 64, 700];

#[tokio::test]
async fn the_blockmap_walk_finds_what_the_engine_finds() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_traverse").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let map = read_map(&fixture).await;
    let traces = traces();
    assert!(traces.len() > 100, "the fan is worth running");

    let sql = format!(
        "WITH\n{}\nSELECT {} AS hits",
        maputl::constants(&db)
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        maputl::path_traverse(&format!(
            "[{}]",
            traces
                .iter()
                .map(|(x1, y1, x2, y2)| maputl::tracing(
                    &x1.to_string(),
                    &y1.to_string(),
                    &x2.to_string(),
                    &y2.to_string()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    );
    let ours: Hits = fixture.scalar(&sql).await;
    assert_eq!(ours.hits.len(), traces.len());

    let mut crossed = 0;
    for (at, (x1, y1, x2, y2)) in traces.iter().enumerate() {
        let want = map.traverse(*x1, *y1, *x2, *y2);
        let got: Vec<(i32, i64)> = ours.hits[at]
            .iter()
            .map(|(line, frac)| (*line, i64::from(*frac)))
            .collect();
        assert_eq!(got, want, "the trace from ({x1}, {y1}) to ({x2}, {y2})");
        crossed += want.len();
    }
    assert!(
        crossed > 200,
        "the fan crosses lines rather than empty space: {crossed}"
    );

    fixture.finish().await;
}

#[derive(Row, Deserialize)]
struct Hits {
    hits: Vec<Vec<(i32, i32)>>,
}

/// A fan out of each starting point, at every sixteenth of a turn.
fn traces() -> Vec<(i64, i64, i64, i64)> {
    let mut traces = Vec::new();
    for (x, y) in FROM {
        for reach in REACH {
            for step in 0..16 {
                let angle = std::f64::consts::TAU * f64::from(step) / 16.0;
                let dx = (angle.cos() * (reach * 65536) as f64) as i64;
                let dy = (angle.sin() * (reach * 65536) as f64) as i64;
                traces.push((x, y, x + dx, y + dy));
            }
        }
    }
    traces
}

#[derive(Row, Deserialize)]
struct Header {
    origin_x: i32,
    origin_y: i32,
    columns: u32,
    rows: u32,
}

#[derive(Row, Deserialize)]
struct Block {
    lines: Vec<u16>,
}

#[derive(Row, Deserialize)]
struct Line {
    v1x: i32,
    v1y: i32,
    v2x: i32,
    v2y: i32,
    dx: i32,
    dy: i32,
}

async fn read_map(fixture: &Fixture) -> Map {
    let db = &fixture.database;
    let header: Header = fixture
        .scalar(&format!(
            "SELECT origin_x, origin_y, columns, rows FROM {db}.lv_blockmap_header LIMIT 1"
        ))
        .await;
    let blocks: Vec<Block> = fixture
        .rows(&format!("SELECT lines FROM {db}.lv_blockmap ORDER BY cell"))
        .await;
    let lines: Vec<Line> = fixture
        .rows(&format!(
            "SELECT a.x AS v1x, a.y AS v1y, b.x AS v2x, b.y AS v2y, l.dx AS dx, l.dy AS dy \
             FROM {db}.lv_lines AS l \
             INNER JOIN {db}.lv_vertexes AS a ON a.id = l.v1 \
             INNER JOIN {db}.lv_vertexes AS b ON b.id = l.v2 \
             ORDER BY l.id"
        ))
        .await;
    Map {
        orgx: i64::from(header.origin_x),
        orgy: i64::from(header.origin_y),
        cols: i64::from(header.columns),
        rows: i64::from(header.rows),
        blocks: blocks
            .into_iter()
            .map(|block| block.lines.into_iter().map(i32::from).collect())
            .collect(),
        v1x: lines.iter().map(|l| i64::from(l.v1x)).collect(),
        v1y: lines.iter().map(|l| i64::from(l.v1y)).collect(),
        v2x: lines.iter().map(|l| i64::from(l.v2x)).collect(),
        v2y: lines.iter().map(|l| i64::from(l.v2y)).collect(),
        dx: lines.iter().map(|l| i64::from(l.dx)).collect(),
        dy: lines.iter().map(|l| i64::from(l.dy)).collect(),
    }
}

/// A trace of no length at all is one the walk still has to answer for.
#[allow(dead_code)]
const _ZERO: i64 = FRACUNIT;
