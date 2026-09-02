//! The frame transform against a real ClickHouse server.
//!
//! Five frames are rendered from the probed game states they were drawn from
//! and compared, pixel by pixel, against the framebuffers the real engine
//! drew. The oracle
//! is the reference emulator's own dump; `native/tests/fixtures/README.md`
//! says where it came from.
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
    /// How many passes of the melt have run by this frame, and zero once the
    /// melt is over. A melt frame draws over the screen the wipe started
    /// from, not over the frame before it, so it has no previous frame to
    /// stand on.
    melt_step: u8,
}

const CASES: [Case; 5] = [
    Case {
        frame: 0,
        tic: 2,
        previous: 0,
        melt_step: 1,
    },
    Case {
        frame: 20,
        tic: 2,
        previous: 0,
        melt_step: 22,
    },
    Case {
        frame: 40,
        tic: 3,
        previous: 39,
        melt_step: 0,
    },
    Case {
        frame: 110,
        tic: 73,
        previous: 109,
        melt_step: 0,
    },
    Case {
        frame: 1000,
        tic: 963,
        previous: 999,
        melt_step: 0,
    },
];

const STATES: &[u8] = include_bytes!("fixtures/demo3-states.tsv");

/// Each fixture frame, with the hash `spec::fb_hash` gives it. A fixture that
/// was replaced by something else fails here rather than passing quietly.
const FRAMES: [(u32, &[u8], &[u8], u64); 8] = [
    (
        0,
        include_bytes!("fixtures/demo3-frame0-fb.bin"),
        include_bytes!("fixtures/demo3-frame0-palette.bin"),
        0xfe5d_82c0_f42d_45f1,
    ),
    (
        20,
        include_bytes!("fixtures/demo3-frame20-fb.bin"),
        include_bytes!("fixtures/demo3-frame20-palette.bin"),
        0x5609_b242_d753_d5d6,
    ),
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
        109,
        include_bytes!("fixtures/demo3-frame109-fb.bin"),
        include_bytes!("fixtures/demo3-frame109-palette.bin"),
        0x0efa_da37_fbd0_c792,
    ),
    (
        110,
        include_bytes!("fixtures/demo3-frame110-fb.bin"),
        include_bytes!("fixtures/demo3-frame110-palette.bin"),
        0xffca_3225_ffc1_4b77,
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
        every_pixel_is_the_one_the_engine_drew(&fixture, case).await;
        the_frame_hashes_to_what_the_probe_recorded(&fixture, case).await;
        the_frame_carries_its_own_bytes_and_colours(&fixture, case).await;
    }

    fixture.finish().await;
}

/// Renders one frame over the one before it. The frame before comes from the
/// reference rather than from this renderer, so each case stands alone.
///
/// The widget cache it carries is what the engine's own would hold at a frame
/// this far into a level: the arms background is down, and every icon reads as
/// not yet drawn, which makes each of them draw itself again over the picture
/// it already put there.
async fn render(fixture: &Fixture, case: &Case) {
    let db = &fixture.database;
    let previous = case.previous;
    let mut plan = Vec::new();
    if case.melt_step == 0 {
        plan.push(Statement::sql(format!(
            "INSERT INTO {db}.native_frames \
             SELECT {previous}, 0, fb, \
             arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, 64001)), \
             palette, 0, '', xxHash64(concat(fb, palette)), 0, \
             CAST((toInt32(0), toInt32(0), toInt32(0), toInt32(0), \
             CAST([], 'Array(Int32)'), CAST([], 'Array(Int32)'), \
             CAST([-1, -1, -1, -1, -1, -1], 'Array(Int32)'), \
             CAST([-1, -1, -1], 'Array(Int32)'), toInt32(-1), toInt32(1)), \
             'Tuple(ready Int32, frags Int32, health Int32, armor Int32, ammo Array(Int32), \
             maxammo Array(Int32), arms Array(Int32), keyboxes Array(Int32), faceindex Int32, \
             armsbg Int32)') \
             FROM {db}.ref_frames WHERE frame = {previous}"
        )));
    }
    plan.push(Statement::sql(render::frame_transform_over(
        db,
        &format!(
            "(SELECT toUInt32({}) AS frame, toUInt32({}) AS tic, toUInt8({}) AS melt_step)",
            case.frame, case.tic, case.melt_step
        ),
    )));
    fixture.execute(&plan).await.expect("the frame renders");
}

/// The whole 320 by 200 frame, byte for byte.
async fn every_pixel_is_the_one_the_engine_drew(fixture: &Fixture, case: &Case) {
    let screen = difference(fixture, case, Region::Screen).await;
    assert_eq!(
        screen.differing, 0,
        "frame {} differs in {} pixels, first at ({}, {}): {} against {}",
        case.frame, screen.differing, screen.x, screen.y, screen.ours, screen.theirs
    );
}

/// `spec::fb_hash` over the framebuffer and the palette, which is what the
/// probe wrote beside the state row this frame was drawn from.
async fn the_frame_hashes_to_what_the_probe_recorded(fixture: &Fixture, case: &Case) {
    let db = &fixture.database;
    let frame = case.frame;
    let ours: u64 = fixture
        .scalar(&format!(
            "SELECT fb_hash FROM {db}.native_frames WHERE frame = {frame}"
        ))
        .await;
    let want = FRAMES
        .iter()
        .find(|(f, _, _, _)| *f == frame)
        .map(|(_, _, _, h)| *h)
        .expect("the frame is in the fixture");
    assert_eq!(
        ours, want,
        "frame {frame} hashes to {ours:016x}, not {want:016x}"
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
