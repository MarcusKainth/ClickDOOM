//! The frame transform against a real ClickHouse server.
//!
//! One frame is rendered from one probed game state and compared, pixel by
//! pixel, against the framebuffer the real engine drew. The oracle is the
//! reference emulator's own dump; `native/tests/fixtures/README.md` says
//! where it came from.
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

/// The frame the fixture holds, and the tic the engine drew it from.
const FRAME: u32 = 40;
const TIC: u32 = 3;

/// The pixels of frame 40's view that the reference draws over the walls,
/// flats and sky: the lamp, the object on the floor to the left, and the
/// four words of the message across the top. Nothing else differs, so this
/// number moves the moment a wall, a flat or a sky pixel does.
const OVERDRAWN: u64 = 1838;

const STATE_TSV: &[u8] = include_bytes!("fixtures/demo3-tic3.tsv");
const FRAME39_FB: &[u8] = include_bytes!("fixtures/demo3-frame39-fb.bin");
const FRAME39_PAL: &[u8] = include_bytes!("fixtures/demo3-frame39-palette.bin");
const FRAME40_FB: &[u8] = include_bytes!("fixtures/demo3-frame40-fb.bin");
const FRAME40_PAL: &[u8] = include_bytes!("fixtures/demo3-frame40-palette.bin");

/// The hashes `spec::fb_hash` gives the two fixture frames. A fixture that
/// was replaced by something else fails here rather than passing quietly.
const FRAME39_HASH: u64 = 0xcd922a65a5e95c23;
const FRAME40_HASH: u64 = 0x2eb87849ee6d9714;

#[derive(Row, Deserialize)]
struct Difference {
    differing: u64,
    x: i32,
    y: i32,
    ours: i32,
    theirs: i32,
}

#[tokio::test]
async fn a_rendered_frame_draws_the_walls_flats_and_sky_the_engine_drew() {
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

    support::probe::load(&fixture, STATE_TSV).await;
    load_reference_frames(&fixture).await;

    the_resident_statement_keeps_frame_zero(&fixture).await;

    fixture
        .execute(&[Statement::sql(render::frame_transform_over(
            &fixture.database,
            &format!(
                "(SELECT toUInt32({FRAME}) AS frame, toUInt32({TIC}) AS tic, \
                 toUInt8(0) AS melt_step)"
            ),
        ))])
        .await
        .expect("the frame renders");

    the_status_bar_is_the_frame_before_it(&fixture).await;
    the_view_matches_everywhere_nothing_was_drawn_over(&fixture).await;
    the_frame_carries_its_own_bytes_and_colours(&fixture).await;

    fixture.finish().await;
}

/// The renderer draws nothing below the view, so those rows have to come
/// through from the frame before unchanged.
async fn the_status_bar_is_the_frame_before_it(fixture: &Fixture) {
    let bar = difference(fixture, Region::StatusBar).await;
    assert_eq!(
        bar.differing, 0,
        "the status bar differs first at ({}, {}): {} against {}",
        bar.x, bar.y, bar.ours, bar.theirs
    );
}

/// Everything in the view is a wall, a flat or the sky, and every one of
/// them matches. What is left over is what the engine draws afterwards and
/// this does not: the sprites and the message line.
async fn the_view_matches_everywhere_nothing_was_drawn_over(fixture: &Fixture) {
    let view = difference(fixture, Region::View).await;
    assert_eq!(
        view.differing, OVERDRAWN,
        "the view differs in {} pixels, first at ({}, {}): {} against {}",
        view.differing, view.x, view.y, view.ours, view.theirs
    );
}

/// `fb_bytes` and `rgb32` are the same frame in two other shapes, and
/// `fb_hash` is what the reference's own frame hashes to once the two
/// sprites and the message are in it.
async fn the_frame_carries_its_own_bytes_and_colours(fixture: &Fixture) {
    let db = &fixture.database;
    let sizes: Vec<(u64, u64, u64)> = fixture
        .rows(&format!(
            "SELECT length(fb), length(fb_bytes), length(rgb32) \
             FROM {db}.native_frames WHERE frame = {FRAME}"
        ))
        .await;
    assert_eq!(sizes, vec![(64000, 64000, 256000)]);

    let same: u8 = fixture
        .scalar(&format!(
            "SELECT toUInt8(arrayStringConcat(arrayMap(c -> char(c), fb_bytes), '') = fb) \
             FROM {db}.native_frames WHERE frame = {FRAME}"
        ))
        .await;
    assert_eq!(same, 1, "fb_bytes is not fb");
}

async fn difference(fixture: &Fixture, region: Region) -> Difference {
    let sql = parity::first_difference(&fixture.database, region)
        .replace("{frame:UInt32}", &FRAME.to_string());
    fixture
        .rows::<Difference>(&sql)
        .await
        .pop()
        .expect("the comparison returns a row")
}

/// Puts frame 39 where the renderer reads the frame before, and frame 40
/// where the comparison reads the reference.
async fn load_reference_frames(fixture: &Fixture) {
    let db = &fixture.database;
    let mut plan = Vec::new();
    for (frame, fb, palette) in [
        (39u32, FRAME39_FB, FRAME39_PAL),
        (FRAME, FRAME40_FB, FRAME40_PAL),
    ] {
        let mut body = Vec::new();
        sql::rowbinary::u32(&mut body, frame);
        sql::rowbinary::string(&mut body, fb);
        sql::rowbinary::string(&mut body, palette);
        plan.push(Statement::data(
            format!("INSERT INTO {db}.ref_frames (frame, fb, palette) FORMAT RowBinary"),
            body,
        ));
    }
    // The frame before, in the shape the renderer reads it back in.
    plan.push(Statement::sql(format!(
        "INSERT INTO {db}.native_frames \
         SELECT 39, 2, fb, \
         arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, 64001)), \
         palette, 0, '', xxHash64(concat(fb, palette)), 0, \
         CAST((0, 0, 0, 0, [], [], [], [], 0), \
         'Tuple(ready Int32, frags Int32, health Int32, armor Int32, ammo Array(Int32), \
         maxammo Array(Int32), arms Array(Int32), keyboxes Array(Int32), faceindex Int32)') \
         FROM {db}.ref_frames WHERE frame = 39"
    )));
    fixture
        .execute(&plan)
        .await
        .expect("the reference frames load");

    let hashes: Vec<(u32, u64)> = fixture
        .rows(&format!(
            "SELECT frame, xxHash64(concat(fb, palette)) FROM {db}.ref_frames ORDER BY frame"
        ))
        .await;
    assert_eq!(hashes, vec![(39, FRAME39_HASH), (FRAME, FRAME40_HASH)]);
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
    sql::rowbinary::u32(&mut body, TIC);
    sql::rowbinary::u8(&mut body, 0);
    sql::rowbinary::string(&mut body, b"");

    let statement = format!("{} FORMAT RowBinary", render::frame_transform(db));
    support::db::run_in_body(db, &statement, &body).expect("the resident statement runs");

    let frames: Vec<u32> = fixture
        .rows(&format!(
            "SELECT frame FROM {db}.native_frames WHERE tic = {TIC} ORDER BY frame"
        ))
        .await;
    assert_eq!(
        frames,
        vec![0],
        "the padding row was drawn, or frame 0 was not"
    );
}
