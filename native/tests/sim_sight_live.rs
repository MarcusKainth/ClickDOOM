//! `P_CheckSight` against a real ClickHouse server.
//!
//! The SQL crosses the BSP without a walk, so it is checked on its own
//! against `native/tests/support/sight.rs`, a reader written from
//! `p_sight.c` that does walk it, with `validcount` and the early returns.
//! The pairs are `E1M7`'s own things looking at each other, which is what
//! `A_Look` and `A_Chase` ask about.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{maputl, sight};
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;

/// How tall the things looking at each other stand, which is a zombieman's
/// height and reach.
const HEIGHT: i64 = 56 << 16;

/// How many of the level's things do the looking, spread evenly through
/// the things lump, and how many of the ones nearest each looks at.
///
/// The nearest are what a monster's `A_Look` asks about, and they mix what
/// stands in the same room with what stands behind its walls.
const VIEWERS: usize = 24;
const TARGETS: usize = 24;

#[tokio::test]
async fn the_sight_check_sees_what_the_engine_sees() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_sight").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let map = read_map(&fixture).await;
    let things = things(&fixture, &map).await;
    assert!(
        things.len() >= 2 * VIEWERS,
        "the level carries enough things to look at each other"
    );
    let mut pairs: Vec<(usize, support::sight::Thing, usize, support::sight::Thing)> = Vec::new();
    let stride = things.len() / VIEWERS;
    for from in things.iter().step_by(stride).take(VIEWERS) {
        let mut near: Vec<&support::sight::Thing> = things.iter().collect();
        near.sort_by_key(|to| (to.x - from.x).abs() + (to.y - from.y).abs());
        for to in near.into_iter().take(TARGETS) {
            pairs.push((
                map.point_in_subsector(from.x, from.y),
                *from,
                map.point_in_subsector(to.x, to.y),
                *to,
            ));
        }
    }

    let heights = sight::Heights {
        floorheight: "sec_floorheight",
        ceilingheight: "sec_ceilingheight",
    };
    let mut with: Vec<(String, String)> = maputl::constants(&db);
    with.extend(sight::constants(&db));
    with.push((
        "sec_floorheight".to_owned(),
        table_column(&db, "floorheight"),
    ));
    with.push((
        "sec_ceilingheight".to_owned(),
        table_column(&db, "ceilingheight"),
    ));
    with.extend(sight::seg_openings(&heights));
    let asks: Vec<String> = pairs
        .iter()
        .map(|(s1, t1, s2, t2)| {
            sight::asking(
                &s1.to_string(),
                &t1.x.to_string(),
                &t1.y.to_string(),
                &t1.z.to_string(),
                &t1.height.to_string(),
                &s2.to_string(),
                &t2.x.to_string(),
                &t2.y.to_string(),
                &t2.z.to_string(),
                &t2.height.to_string(),
            )
        })
        .collect();
    let sql = format!(
        "WITH\n{}\nSELECT {} AS seen",
        with.iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        sight::check_sight(&format!("[{}]", asks.join(", ")))
    );
    let ours: Seen = fixture.scalar(&sql).await;
    fixture.finish().await;

    assert_eq!(ours.seen.len(), pairs.len());
    let mut visible = 0;
    for (at, (_, t1, _, t2)) in pairs.iter().enumerate() {
        let want = map.check_sight(*t1, *t2);
        assert_eq!(
            ours.seen[at] == 1,
            want,
            "({}, {}) looking at ({}, {})",
            t1.x,
            t1.y,
            t2.x,
            t2.y
        );
        visible += usize::from(want);
    }
    // A fan that answered the same way every time would pass whatever the
    // expression said.
    assert!(
        visible > 20 && visible < pairs.len() - 20,
        "the fan has to see and be blocked: {visible} of {}",
        pairs.len()
    );
}

#[derive(Row, Deserialize)]
struct Seen {
    seen: Vec<u8>,
}

fn table_column(db: &str, column: &str) -> String {
    format!(
        "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((id, {column}))))\
         \n     FROM {db}.lv_sectors_static)"
    )
}

/// The level's own things, standing on the floor of the sector they spawn
/// in, which is where `P_SpawnMapThing` puts them.
async fn things(fixture: &Fixture, map: &support::sight::Map) -> Vec<support::sight::Thing> {
    let db = &fixture.database;
    let rows: Vec<Spawn> = fixture
        .rows(&format!(
            "SELECT toInt32(x) AS x, toInt32(y) AS y FROM {db}.lv_things ORDER BY id"
        ))
        .await;
    rows.into_iter()
        .map(|spawn| {
            let x = i64::from(spawn.x) << 16;
            let y = i64::from(spawn.y) << 16;
            let sector = map.ssec_sector[map.point_in_subsector(x, y)];
            support::sight::Thing {
                x,
                y,
                z: map.floorheight[sector],
                height: HEIGHT,
            }
        })
        .collect()
}

#[derive(Row, Deserialize)]
struct Spawn {
    x: i32,
    y: i32,
}

#[derive(Row, Deserialize)]
struct Node {
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    children: Vec<u16>,
}

#[derive(Row, Deserialize)]
struct Subsector {
    firstline: u32,
    numlines: u32,
    sector: u32,
}

#[derive(Row, Deserialize)]
struct Seg {
    linedef: u32,
    frontsector: i32,
    backsector: i32,
}

#[derive(Row, Deserialize)]
struct Line {
    v1x: i32,
    v1y: i32,
    v2x: i32,
    v2y: i32,
    dx: i32,
    dy: i32,
    flags: i16,
}

#[derive(Row, Deserialize)]
struct Sector {
    floorheight: i32,
    ceilingheight: i32,
}

#[derive(Row, Deserialize)]
struct Reject {
    bits: Vec<u8>,
    sectors: u64,
}

async fn read_map(fixture: &Fixture) -> support::sight::Map {
    let db = &fixture.database;
    let nodes: Vec<Node> = fixture
        .rows(&format!(
            "SELECT x, y, dx, dy, children FROM {db}.lv_nodes ORDER BY id"
        ))
        .await;
    let subsectors: Vec<Subsector> = fixture
        .rows(&format!(
            "SELECT firstline, numlines, sector FROM {db}.lv_subsectors ORDER BY id"
        ))
        .await;
    let segs: Vec<Seg> = fixture
        .rows(&format!(
            "SELECT linedef, frontsector, backsector FROM {db}.lv_segs ORDER BY id"
        ))
        .await;
    let lines: Vec<Line> = fixture
        .rows(&format!(
            "SELECT a.x AS v1x, a.y AS v1y, b.x AS v2x, b.y AS v2y, \
             l.dx AS dx, l.dy AS dy, l.flags AS flags \
             FROM {db}.lv_lines AS l \
             INNER JOIN {db}.lv_vertexes AS a ON a.id = l.v1 \
             INNER JOIN {db}.lv_vertexes AS b ON b.id = l.v2 \
             ORDER BY l.id"
        ))
        .await;
    let sectors: Vec<Sector> = fixture
        .rows(&format!(
            "SELECT floorheight, ceilingheight FROM {db}.lv_sectors_static ORDER BY id"
        ))
        .await;
    let reject: Reject = fixture
        .scalar(&format!(
            "SELECT arrayMap(i -> reinterpretAsUInt8(substring(bits, i, 1)), \
             range(1, 1 + length(bits))) AS bits, \
             assumeNotNull(toUInt64((SELECT count() FROM {db}.lv_sectors_static))) AS sectors \
             FROM {db}.lv_reject LIMIT 1"
        ))
        .await;
    support::sight::Map {
        node_x: nodes.iter().map(|n| i64::from(n.x)).collect(),
        node_y: nodes.iter().map(|n| i64::from(n.y)).collect(),
        node_dx: nodes.iter().map(|n| i64::from(n.dx)).collect(),
        node_dy: nodes.iter().map(|n| i64::from(n.dy)).collect(),
        node_child: nodes
            .iter()
            .map(|n| [u32::from(n.children[0]), u32::from(n.children[1])])
            .collect(),
        subsector: subsectors
            .iter()
            .map(|s| (s.firstline as usize, s.numlines as usize))
            .collect(),
        ssec_sector: subsectors.iter().map(|s| s.sector as usize).collect(),
        seg_line: segs.iter().map(|s| s.linedef as usize).collect(),
        seg_front: segs.iter().map(|s| i64::from(s.frontsector)).collect(),
        seg_back: segs.iter().map(|s| i64::from(s.backsector)).collect(),
        line_v1x: lines.iter().map(|l| i64::from(l.v1x)).collect(),
        line_v1y: lines.iter().map(|l| i64::from(l.v1y)).collect(),
        line_v2x: lines.iter().map(|l| i64::from(l.v2x)).collect(),
        line_v2y: lines.iter().map(|l| i64::from(l.v2y)).collect(),
        line_dx: lines.iter().map(|l| i64::from(l.dx)).collect(),
        line_dy: lines.iter().map(|l| i64::from(l.dy)).collect(),
        line_flags: lines.iter().map(|l| i64::from(l.flags)).collect(),
        floorheight: sectors.iter().map(|s| i64::from(s.floorheight)).collect(),
        ceilingheight: sectors.iter().map(|s| i64::from(s.ceilingheight)).collect(),
        reject: reject.bits,
        numsectors: reject.sectors as usize,
    }
}
