//! The frame transform against a real ClickHouse server.
//!
//! Two frames are rendered from two probed game states and compared, pixel by
//! pixel, against the framebuffers the real engine drew. The oracle is the
//! reference emulator's own dump; `native/tests/fixtures/README.md` says where
//! it came from.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::parity::{self, Region};
use clickdoom_native::sql::{self, Statement, render};
use clickdoom_native::{load, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;

/// One frame of demo3, with the frame it draws over.
struct Case {
    /// The frame drawn, the tic the engine drew it from, and the frame before.
    frame: u32,
    tic: u32,
    previous: u32,
    /// What the reference draws over the view that this does not: the words of
    /// the heads-up message across the top. Every other pixel of the view
    /// matches, so this number moves the moment a wall, a flat, the sky or a
    /// thing does.
    overdrawn: u64,
    /// How much of the status bar the frame before no longer carries. The
    /// engine redraws it every frame, and a widget that changed differs until
    /// the status bar is drawn here too.
    status_bar: u64,
}

const CASES: [Case; 2] = [
    Case {
        frame: 40,
        tic: 3,
        previous: 39,
        overdrawn: 591,
        status_bar: 0,
    },
    Case {
        frame: 1000,
        tic: 963,
        previous: 999,
        overdrawn: 993,
        status_bar: 140,
    },
];

const STATES: &[u8] = include_bytes!("fixtures/demo3-states.tsv");

/// Each fixture frame, with the hash `spec::fb_hash` gives it. A fixture that
/// was replaced by something else fails here rather than passing quietly.
const FRAMES: [(u32, &[u8], &[u8], u64); 4] = [
    (
        39,
        include_bytes!("fixtures/demo3-frame39-fb.bin"),
        include_bytes!("fixtures/demo3-frame39-palette.bin"),
        0xcd92_2a65_a5e9_5c23,
    ),
    (
        40,
        include_bytes!("fixtures/demo3-frame40-fb.bin"),
        include_bytes!("fixtures/demo3-frame40-palette.bin"),
        0x2eb8_7849_ee6d_9714,
    ),
    (
        999,
        include_bytes!("fixtures/demo3-frame999-fb.bin"),
        include_bytes!("fixtures/demo3-frame999-palette.bin"),
        0x0fe2_7f3c_06fb_13ba,
    ),
    (
        1000,
        include_bytes!("fixtures/demo3-frame1000-fb.bin"),
        include_bytes!("fixtures/demo3-frame1000-palette.bin"),
        0x0153_9e68_8afb_3350,
    ),
];

#[derive(Row, Deserialize)]
struct Difference {
    differing: u64,
    x: i32,
    y: i32,
    ours: i32,
    theirs: i32,
}

#[tokio::test]
async fn a_rendered_frame_draws_what_the_engine_drew() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("render").await;

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

    support::probe::load(&fixture, STATES).await;
    load_reference_frames(&fixture).await;

    the_resident_statement_keeps_frame_zero(&fixture).await;

    for case in &CASES {
        render(&fixture, case).await;
        the_view_matches_everywhere_nothing_was_drawn_over(&fixture, case).await;
        the_status_bar_is_the_frame_before_it(&fixture, case).await;
        the_frame_carries_its_own_bytes_and_colours(&fixture, case).await;
    }

    fixture.finish().await;
}

/// Renders one frame over the one before it. The frame before comes from the
/// reference rather than from this renderer, so each case stands alone.
async fn render(fixture: &Fixture, case: &Case) {
    let db = &fixture.database;
    let previous = case.previous;
    let plan = [
        Statement::sql(format!(
            "INSERT INTO {db}.native_frames \
             SELECT {previous}, 0, fb, \
             arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, 64001)), \
             palette, 0, '', xxHash64(concat(fb, palette)), 0, \
             CAST((0, 0, 0, 0, [], [], [], [], 0), \
             'Tuple(ready Int32, frags Int32, health Int32, armor Int32, ammo Array(Int32), \
             maxammo Array(Int32), arms Array(Int32), keyboxes Array(Int32), faceindex Int32)') \
             FROM {db}.ref_frames WHERE frame = {previous}"
        )),
        Statement::sql(render::frame_transform_over(
            db,
            &format!(
                "(SELECT toUInt32({}) AS frame, toUInt32({}) AS tic, toUInt8(0) AS melt_step)",
                case.frame, case.tic
            ),
        )),
    ];
    fixture.execute(&plan).await.expect("the frame renders");
}

/// Everything in the view is a wall, a flat, the sky or a thing, and every one
/// of them matches. What is left over is what the engine draws afterwards and
/// this does not.
async fn the_view_matches_everywhere_nothing_was_drawn_over(fixture: &Fixture, case: &Case) {
    let view = difference(fixture, case, Region::View).await;
    assert_eq!(
        view.differing, case.overdrawn,
        "frame {} differs in {} view pixels, first at ({}, {}): {} against {}",
        case.frame, view.differing, view.x, view.y, view.ours, view.theirs
    );
}

/// The renderer draws nothing below the view, so those rows come through from
/// the frame before unchanged.
async fn the_status_bar_is_the_frame_before_it(fixture: &Fixture, case: &Case) {
    let bar = difference(fixture, case, Region::StatusBar).await;
    assert_eq!(
        bar.differing, case.status_bar,
        "frame {}'s status bar differs in {} pixels, first at ({}, {}): {} against {}",
        case.frame, bar.differing, bar.x, bar.y, bar.ours, bar.theirs
    );
}

/// `fb_bytes` and `rgb32` are the same frame in two other shapes.
async fn the_frame_carries_its_own_bytes_and_colours(fixture: &Fixture, case: &Case) {
    let db = &fixture.database;
    let frame = case.frame;
    let sizes: Vec<(u64, u64, u64)> = fixture
        .rows(&format!(
            "SELECT length(fb), length(fb_bytes), length(rgb32) \
             FROM {db}.native_frames WHERE frame = {frame}"
        ))
        .await;
    assert_eq!(sizes, vec![(64000, 64000, 256000)]);

    let same: u8 = fixture
        .scalar(&format!(
            "SELECT toUInt8(arrayStringConcat(arrayMap(c -> char(c), fb_bytes), '') = fb) \
             FROM {db}.native_frames WHERE frame = {frame}"
        ))
        .await;
    assert_eq!(same, 1, "frame {frame}: fb_bytes is not fb");
}

async fn difference(fixture: &Fixture, case: &Case, region: Region) -> Difference {
    let sql = parity::first_difference(&fixture.database, region)
        .replace("{frame:UInt32}", &case.frame.to_string());
    fixture
        .rows::<Difference>(&sql)
        .await
        .pop()
        .expect("the comparison returns a row")
}

/// Puts every fixture frame where the comparison reads the reference.
async fn load_reference_frames(fixture: &Fixture) {
    let db = &fixture.database;
    let mut plan = Vec::new();
    for (frame, fb, palette, _) in FRAMES {
        let mut body = Vec::new();
        sql::rowbinary::u32(&mut body, frame);
        sql::rowbinary::string(&mut body, fb);
        sql::rowbinary::string(&mut body, palette);
        plan.push(Statement::data(
            format!("INSERT INTO {db}.ref_frames (frame, fb, palette) FORMAT RowBinary"),
            body,
        ));
    }
    fixture
        .execute(&plan)
        .await
        .expect("the reference frames load");

    let hashes: Vec<(u32, u64)> = fixture
        .rows(&format!(
            "SELECT frame, xxHash64(concat(fb, palette)) FROM {db}.ref_frames ORDER BY frame"
        ))
        .await;
    let want: Vec<(u32, u64)> = FRAMES.iter().map(|(f, _, _, h)| (*f, *h)).collect();
    assert_eq!(hashes, want);
}

/// The statement the resident pipeline runs, driven over its own `input()`
/// with the padding row the server pre-reads in front of a real one.
///
/// Frame 0 is the melt's first frame, so the padding row cannot be told apart
/// by its frame number. What tells them apart is that a real row leaves `pad`
/// empty.
async fn the_resident_statement_keeps_frame_zero(fixture: &Fixture) {
    let db = &fixture.database;
    let mut body = Vec::new();
    // The padding row: the bytes the server reads before it parses.
    sql::rowbinary::u32(&mut body, 0);
    sql::rowbinary::u32(&mut body, 0);
    sql::rowbinary::u8(&mut body, 0);
    sql::rowbinary::string(&mut body, &[b'.'; 128]);
    // A real row, for the frame the padding row shares a number with.
    sql::rowbinary::u32(&mut body, 0);
    sql::rowbinary::u32(&mut body, CASES[0].tic);
    sql::rowbinary::u8(&mut body, 0);
    sql::rowbinary::string(&mut body, b"");

    let statement = format!("{} FORMAT RowBinary", render::frame_transform(db));
    support::db::run_in_body(db, &statement, &body).expect("the resident statement runs");

    let frames: Vec<u32> = fixture
        .rows(&format!(
            "SELECT frame FROM {db}.native_frames WHERE tic = {} ORDER BY frame",
            CASES[0].tic
        ))
        .await;
    assert_eq!(
        frames,
        vec![0],
        "the padding row was drawn, or frame 0 was not"
    );
}
