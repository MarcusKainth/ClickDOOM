//! The level decode against a real ClickHouse server.
//!
//! Everything checked here is produced by `native/sql/level_load.sql`. The
//! test's own decoders read the WAD a second time, independently, and the
//! two agreeing is the point: a shared decoder would only prove the SQL
//! matches itself.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::patch;

/// Loads the WAD and decodes `E1M7` once, then runs every check against
/// the one database. The load is the expensive part and nothing here
/// writes, so a database per check would only pay for it again.
#[tokio::test]
async fn a_loaded_level_holds_what_the_wad_says() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("level").await;
    let mut plan = load::plan(&fixture.database, &wad);
    plan.extend(sql::level_statements(
        &fixture.database,
        support::MAP,
        support::DEMO,
    ));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    every_map_table_holds_one_row_per_record(&fixture, &wad).await;
    the_derived_map_fields_point_at_rows_that_exist(&fixture).await;
    the_bsp_paths_reach_every_subsector(&fixture).await;
    the_composed_textures_match_a_decoder_written_from_the_engine(&fixture, &wad).await;
    every_texture_window_holds_the_bytes_its_column_reads(&fixture, &wad).await;
    the_demo_decodes_to_its_tic_commands(&fixture, &wad).await;
    the_assets_are_numbered_the_way_the_engine_numbers_them(&fixture, &wad).await;

    fixture.finish().await;
}

/// `(table, map lump, record size)`. The row count has to be the lump's
/// own size divided by the record size, which is what says the decode read
/// whole records and lost none.
const RECORDS: [(&str, &str, usize); 8] = [
    ("lv_vertexes", "VERTEXES", 4),
    ("lv_lines", "LINEDEFS", 14),
    ("lv_sides", "SIDEDEFS", 30),
    ("lv_sectors_static", "SECTORS", 26),
    ("lv_segs", "SEGS", 12),
    ("lv_subsectors", "SSECTORS", 4),
    ("lv_nodes", "NODES", 28),
    ("lv_things", "THINGS", 10),
];

async fn every_map_table_holds_one_row_per_record(fixture: &Fixture, wad: &Wad<'_>) {
    {
        for (table, lump, size) in RECORDS {
            let expected = wad.map_lump(support::MAP, lump).unwrap().bytes.len() / size;
            assert_eq!(fixture.count(table).await, expected as u64, "{table}");
        }
        // The blockmap's cell count is its own header's, and the reject
        // matrix is one bit per ordered sector pair.
        let sectors = fixture.count("lv_sectors_static").await as usize;
        let (columns, rows): (u32, u32) = fixture
            .scalar(&format!(
                "SELECT columns, rows FROM {}.lv_blockmap_header",
                fixture.database
            ))
            .await;
        assert_eq!(
            fixture.count("lv_blockmap").await,
            u64::from(columns) * u64::from(rows)
        );
        let bits: u64 = fixture
            .scalar(&format!(
                "SELECT length(bits) FROM {}.lv_reject",
                fixture.database
            ))
            .await;
        assert_eq!(bits as usize, (sectors * sectors).div_ceil(8));
    }
}

/// `P_LoadSegs` and `P_GroupLines` fill in what the lumps do not carry, so
/// these are the values a wrong join would get wrong without changing a
/// single row count.
async fn the_derived_map_fields_point_at_rows_that_exist(fixture: &Fixture) {
    {
        let db = &fixture.database;
        let sectors = fixture.count("lv_sectors_static").await as i32;
        let (min_front, max_front): (i32, i32) = fixture
            .scalar(&format!(
                "SELECT min(frontsector), max(frontsector) FROM {db}.lv_segs"
            ))
            .await;
        assert_eq!(min_front, 0);
        assert!(max_front < sectors);
        // A one-sided seg has no sector behind it, and a two-sided one
        // has a real sector there.
        let bad: u64 = fixture
            .scalar(&format!(
                "SELECT count() FROM {db}.lv_segs WHERE backsector < -1 OR backsector >= {sectors}"
            ))
            .await;
        assert_eq!(bad, 0);
        // Every subsector's sector is the one its first seg fronts.
        let mismatched: u64 = fixture
            .scalar(&format!(
                "SELECT count() FROM {db}.lv_subsectors ss \
                 INNER JOIN {db}.lv_segs sg ON sg.id = ss.firstline \
                 WHERE toInt32(ss.sector) != sg.frontsector"
            ))
            .await;
        assert_eq!(mismatched, 0);
        // Every sector was given the lines that touch it.
        let empty: u64 = fixture
            .scalar(&format!(
                "SELECT count() FROM {db}.lv_sectors_static WHERE empty(lines)"
            ))
            .await;
        assert_eq!(empty, 0);
    }
}

/// Every subsector's path starts at the BSP root, and the ranges the node
/// table holds cover the subsectors those paths visit.
async fn the_bsp_paths_reach_every_subsector(fixture: &Fixture) {
    {
        let db = &fixture.database;
        let subsectors = fixture.count("lv_subsectors").await;
        assert_eq!(fixture.count("lv_ssec_path").await, subsectors);
        let root: u32 = fixture
            .scalar(&format!("SELECT max(id) FROM {db}.lv_nodes"))
            .await;
        let (first, last): (u32, u32) = fixture
            .scalar(&format!(
                "SELECT first_ssec, last_ssec FROM {db}.lv_node_range WHERE node = {root}"
            ))
            .await;
        assert_eq!((first, u64::from(last) + 1), (0, subsectors));
        assert_eq!(
            fixture.count("lv_node_range").await,
            fixture.count("lv_nodes").await
        );

        // `sides[i]` is the branch taken at `nodes[i]`, so following it
        // has to reach `nodes[i + 1]`, and the last one has to reach the
        // subsector the path belongs to. A path recording the branch that
        // arrived at each node instead passes every check above and fails
        // this one.
        let astray: u64 = fixture
            .scalar(&format!(
                "SELECT count() FROM \
                 ( \
                     SELECT p.subsector AS subsector, p.depth AS depth, j, \
                            p.nodes[j + 1] AS node, p.sides[j + 1] AS side, \
                            if(j + 2 <= p.depth, p.nodes[j + 2], 4294967295) AS next \
                     FROM {db}.lv_ssec_path AS p \
                     ARRAY JOIN range(p.depth) AS j \
                 ) AS e \
                 INNER JOIN {db}.lv_nodes AS n ON n.id = e.node \
                 WHERE n.children[e.side + 1] != \
                       if(e.next = 4294967295, bitOr(e.subsector, 32768), e.next)"
            ))
            .await;
        assert_eq!(astray, 0, "a path's branch does not lead to the next node");
    }
}

#[derive(Row, Deserialize)]
struct ColumnRow {
    col: u16,
    patches: u16,
    lump: i64,
    ofs: u32,
}

/// The textures E1M7 draws, composed in SQL, against a decoder written
/// from `r_data.c`. `STONE` is composed out of 40 patches and every one
/// of its columns needs the composite; `SKY1` is a single patch placed
/// eight rows above the texture top, so its columns read straight out of
/// the patch lump.
const COMPOSED: [&str; 4] = ["STONE", "COMPTALL", "BROWN1", "SKY1"];

async fn the_composed_textures_match_a_decoder_written_from_the_engine(
    fixture: &Fixture,
    wad: &Wad<'_>,
) {
    {
        let db = &fixture.database;
        let pnames = patch::pnames(wad);
        let textures = patch::textures(wad);
        for name in COMPOSED {
            let (id, texture) = textures
                .iter()
                .enumerate()
                .find(|(_, t)| t.name == name)
                .unwrap_or_else(|| panic!("{name} is not in TEXTURE1"));
            let (expected, block) = patch::compose(wad, texture, &pnames);

            let rows: Vec<ColumnRow> = fixture
                .rows(&format!(
                    "SELECT col, patches, toInt64(lump) AS lump, ofs \
                     FROM {db}.tex_columns WHERE texture = {id} ORDER BY col"
                ))
                .await;
            assert_eq!(rows.len(), expected.len(), "{name} column count");
            for (row, want) in rows.iter().zip(&expected) {
                let at = format!("{name} column {}", row.col);
                assert_eq!(row.patches, want.patches, "{at} patch count");
                assert_eq!(row.lump, want.lump, "{at} lump");
                assert_eq!(row.ofs, want.ofs, "{at} offset");
            }

            let composite: String = fixture
                .scalar(&format!(
                    "SELECT ifNull((SELECT lower(hex(data)) FROM {db}.tex_composite \
                     WHERE texture = {id}), '')"
                ))
                .await;
            assert_eq!(composite.len(), block.len() * 2, "{name} composite length");
            assert_eq!(composite, hex(&block), "{name} composite bytes");
        }
    }
}

#[derive(Row, Deserialize)]
struct WindowRow {
    texture: u32,
    col: u16,
    window: String,
    overrun: u8,
}

/// Every texture column's 128-byte window, against the same bytes read out
/// of the WAD. The over-run count is reported because it is the number of
/// columns whose source runs out inside the window, and a change to the
/// window size or to the composite moves it.
async fn every_texture_window_holds_the_bytes_its_column_reads(fixture: &Fixture, wad: &Wad<'_>) {
    {
        let db = &fixture.database;
        let pnames = patch::pnames(wad);
        let textures = patch::textures(wad);
        let rows: Vec<WindowRow> = fixture
            .rows(&format!(
                "SELECT texture, col, lower(hex(window)) AS window, overrun \
                 FROM {db}.tex_window ORDER BY texture, col"
            ))
            .await;
        let width: usize = textures.iter().map(|t| t.width as usize).sum();
        assert_eq!(rows.len(), width);

        let mut overruns = 0;
        let mut at = 0;
        for (id, texture) in textures.iter().enumerate() {
            let (columns, block) = patch::compose(wad, texture, &pnames);
            for (col, column) in columns.iter().enumerate() {
                let row = &rows[at];
                at += 1;
                assert_eq!((row.texture as usize, row.col as usize), (id, col));
                let source: &[u8] = match column.lump {
                    -1 => &block,
                    lump => wad.lumps()[lump as usize].bytes,
                };
                let (want, overrun) = patch::window(source, column.ofs);
                assert_eq!(row.window, hex(&want), "{} column {col}", texture.name);
                assert_eq!(
                    row.overrun,
                    u8::from(overrun),
                    "{} column {col}",
                    texture.name
                );
                overruns += usize::from(overrun);
            }
        }
        println!(
            "tex_window: {} columns, {overruns} over-running",
            rows.len()
        );
        assert_eq!(overruns, 119, "the over-running column count changed");
    }
}

#[derive(Row, Deserialize)]
struct CmdRow {
    tic: u32,
    forwardmove: i8,
    sidemove: i8,
    angleturn: i16,
    buttons: u8,
}

/// DEMO3's tic commands, against the lump's own bytes.
async fn the_demo_decodes_to_its_tic_commands(fixture: &Fixture, wad: &Wad<'_>) {
    {
        let db = &fixture.database;
        let demo = wad.find(support::DEMO).unwrap().bytes;
        let body = &demo[13..demo.len() - 1];
        assert_eq!(body.len() % 4, 0);
        assert_eq!(fixture.count("demo_cmds").await, 2134);
        assert_eq!(body.len() / 4, 2134);

        let rows: Vec<CmdRow> = fixture
            .rows(&format!(
                "SELECT tic, forwardmove, sidemove, angleturn, buttons \
                 FROM {db}.demo_cmds ORDER BY tic"
            ))
            .await;
        for (at, row) in rows.iter().enumerate() {
            let cmd = &body[at * 4..at * 4 + 4];
            assert_eq!(row.tic as usize, at + 1);
            assert_eq!(row.forwardmove, cmd[0] as i8, "tic {}", row.tic);
            assert_eq!(row.sidemove, cmd[1] as i8, "tic {}", row.tic);
            assert_eq!(row.angleturn, i16::from(cmd[2]) << 8, "tic {}", row.tic);
            assert_eq!(row.buttons, cmd[3], "tic {}", row.tic);
        }
        let (version, skill, episode, map): (u8, u8, u8, u8) = fixture
            .scalar(&format!(
                "SELECT version, skill, episode, map FROM {db}.demo_header"
            ))
            .await;
        assert_eq!((version, skill, episode, map), (109, 2, 1, 7));
    }
}

/// The assets the renderer reads: flats numbered the way `R_FlatNumForName`
/// numbers them, the colormaps and the palettes, and every sprite frame
/// with a picture for each of its eight rotations.
async fn the_assets_are_numbered_the_way_the_engine_numbers_them(fixture: &Fixture, wad: &Wad<'_>) {
    {
        let db = &fixture.database;
        let first = wad.find("F_START").unwrap().index + 1;
        let last = wad.find("F_END").unwrap().index - 1;
        assert_eq!(fixture.count("flats").await, u64::from(last - first + 1));
        let (id, name): (u32, String) = fixture
            .scalar(&format!(
                "SELECT id, name FROM {db}.flats WHERE length(data) = 4096 ORDER BY id LIMIT 1"
            ))
            .await;
        assert_eq!(name, wad.lumps()[(first + id) as usize].name);

        assert_eq!(fixture.count("colormap").await, 34);
        assert_eq!(fixture.count("playpal").await, 14);
        let sizes: (u64, u64) = fixture
            .scalar(&format!(
                "SELECT min(length(data)), max(length(data)) FROM {db}.colormap"
            ))
            .await;
        assert_eq!(sizes, (256, 256));

        let sprite_lumps = fixture.count("sprite_lumps").await;
        assert_eq!(
            sprite_lumps,
            u64::from(wad.find("S_END").unwrap().index - wad.find("S_START").unwrap().index - 1)
        );
        let bad: u64 = fixture
            .scalar(&format!(
                "SELECT count() FROM {db}.sprite_frames \
                 WHERE length(lump) != 8 OR length(flip) != 8 \
                 OR arrayExists(x -> x >= {sprite_lumps} OR x < 0, lump)"
            ))
            .await;
        assert_eq!(bad, 0, "a sprite frame names a lump that is not a sprite");
        // Every post's pixels sit where `sprite_posts` says in the pool.
        let (pool, want): (u64, u64) = fixture
            .scalar(&format!(
                "SELECT assumeNotNull((SELECT length(data) FROM {db}.sprite_pool)), \
                 assumeNotNull((SELECT max(pool_ofs + length) FROM {db}.sprite_posts))"
            ))
            .await;
        assert_eq!(pool, want);
    }
}

/// The same WAD, decoded twice into two databases, has to produce the same
/// rows. Several statements group and sort, and `groupArray` does not
/// promise an order, so nothing but a second run says the explicit sorts
/// cover every one of them. The renderer's own tables are in here for the
/// same reason: each pixel pool is one `groupArray` over a sorted read.
#[tokio::test]
async fn two_loads_of_the_same_wad_agree_row_for_row() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let mut digests = Vec::new();
    for case in ["twiceA", "twiceB"] {
        let fixture = Fixture::create(case).await;
        let mut plan = load::plan(&fixture.database, &wad);
        plan.extend(sql::level_statements(
            &fixture.database,
            support::MAP,
            support::DEMO,
        ));
        plan.extend(sql::render_statements(&fixture.database, support::SKY));
        if let Err(error) = fixture.execute(&plan).await {
            fixture.finish().await;
            panic!("{error}");
        }
        digests.push(digest(&fixture).await);
        fixture.finish().await;
    }
    // Every table the schema declares is in the digest, apart from the ones
    // it leaves out by name, so a table the load forgot shows up here.
    let mut want: Vec<&str> = sql::schema_tables()
        .into_iter()
        .filter(|t| !NOT_DIGESTED.contains(t))
        .collect();
    want.sort_unstable();
    let got: Vec<&str> = digests[0].iter().map(|d| d.table.as_str()).collect();
    assert_eq!(
        got, want,
        "the digest does not cover every table the schema declares"
    );
    for (a, b) in digests[0].iter().zip(&digests[1]) {
        assert_eq!(a, b, "{} differs between two loads", a.table);
    }
}

/// Every table's row count and the sum of its rows' hashes. The sum does
/// not depend on the order rows come back in, which is what a comparison
/// between two databases needs.
#[derive(Debug, Eq, PartialEq, Row, Deserialize)]
struct Digest {
    table: String,
    rows: u64,
    hash: u64,
}

/// Tables a load leaves empty or a run fills, which the digest skips.
const NOT_DIGESTED: [&str; 3] = ["native_state", "native_frames", "ref_frames"];

async fn digest(fixture: &Fixture) -> Vec<Digest> {
    let db = &fixture.database;
    let tables: Vec<String> = fixture
        .rows(&format!(
            "SELECT name FROM system.tables WHERE database = '{db}' \
             AND name NOT IN ({}) ORDER BY name",
            NOT_DIGESTED
                .iter()
                .map(|t| format!("'{t}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .await;
    let parts: Vec<String> = tables
        .iter()
        .map(|t| {
            format!(
                "SELECT '{t}' AS table, count() AS rows, sum(cityHash64(*)) AS hash FROM {db}.{t}"
            )
        })
        .collect();
    let mut rows: Vec<Digest> = fixture.rows(&parts.join(" UNION ALL ")).await;
    rows.sort_by(|a, b| a.table.cmp(&b.table));
    rows
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
