//! Live proof for the renderer-only session, run against a real ClickHouse
//! server.
//!
//! `native_session_live.rs` drives two stand-in statements. This drives the
//! real renderer over a real level:
//!
//!   * a session opened without a simulation renders the frames the probe
//!     recorded, from the state rows the probe left in `native_state`, and
//!     each one hashes to what the engine's own frame hashed to;
//!   * the same session has nothing to feed a tic to, and says so;
//!   * the PPM the driver writes is the palette applied to the framebuffer,
//!     checked against an independent re-derivation from the two.
//!
//! All of it is one test function. A renderer statement holds its scalar
//! constants for as long as it is open, `cargo test` runs the functions of
//! one binary in parallel, and two statements open at once ran the server
//! out of memory.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`) and the committed
//! probe fixture.

#![cfg(feature = "clickhouse-tests")]

use std::time::Duration; // purity-ok: a timeout in the harness, never a value a statement reads

use clickdoom_driver::client::Db;
use clickdoom_driver::native::{Frame, Session, SessionError, probe, schedule};
use clickdoom_driver::render::{FB_HEIGHT, FB_WIDTH, ppm_sql_over};
use clickdoom_native::sql;

mod support;

use support::{committed_fixture, conn_args, drop_database, loaded};

/// The first frame a run renders pays for the statement's analysis and its
/// scalar constants, which is seconds.
const FRAME_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::test]
async fn a_renderer_only_session_draws_the_frames_the_engine_drew() {
    let (database, admin) = loaded("render").await;
    probe::load(&admin, &database, &committed_fixture())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let conn = conn_args(&database);

    // The fixture holds the melt's first and last frames and the two after
    // it, then jumps to frame 1000. A frame whose own previous frame is
    // missing has nothing to draw over, and asking for one is refused.
    let refused = schedule::from_probe(&admin, &database, None)
        .await
        .expect_err("the fixture's last frame has nothing to draw over");
    assert!(
        matches!(refused, schedule::Error::NoPreviousFrame { .. }),
        "{refused}"
    );

    let last_contiguous = 41;
    let plan = schedule::from_probe(&admin, &database, Some(last_contiguous))
        .await
        .expect("the probe rows say which frames to draw");
    assert!(
        plan.iter().any(|row| row.melt_step > 0),
        "the fixture carries no melt frame, so this checks nothing about the wipe"
    );

    schedule::clear_frames(&admin, &database)
        .await
        .expect("an empty frames table");
    let session = Session::open(
        &conn,
        &database,
        None,
        Some(&sql::render::frame_transform(&database)),
    )
    .await
    .expect("opening the renderer alone");
    assert!(!session.has_sim());

    // A renderer-only session has nothing to feed a tic to. The state rows
    // it reads are the ones the probe left behind.
    let refused = session
        .feed_sim(1, 0, 0, 0, 0)
        .expect_err("there is no simulation");
    assert!(matches!(refused, SessionError::NoSim { .. }), "{refused}");
    assert_eq!(
        session
            .wait_sim(plan[0].tic, FRAME_TIMEOUT)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Ok(()),
        "the probe's own tics are already committed"
    );

    let mut last = None;
    for row in &plan {
        session
            .feed_render(row.frame, row.tic, row.melt_step)
            .unwrap_or_else(|e| panic!("feeding frame {}: {e}", row.frame));
        let frame = session
            .wait_frame(row.frame, FRAME_TIMEOUT)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .frame;
        assert_eq!(
            frame.fb.len(),
            64_000,
            "frame {} is not a framebuffer",
            row.frame
        );
        assert_eq!(frame.palette.len(), 768, "frame {}", row.frame);
        assert_eq!(frame.rgb32.len(), 256_000, "frame {}", row.frame);
        assert_eq!(
            frame.fb_hash, row.probe_fb_hash,
            "frame {} drew something the engine did not",
            row.frame
        );
        last = Some(frame);
    }
    let last = last.expect("the fixture names a frame to draw");

    session.close().await.expect("the statement finished");

    the_ppm_is_the_palette_applied_to_the_frame(&admin, &database, &last).await;
    drop_database(&admin, &database).await;
}

/// The PPM is built in SQL. This re-derives it from the framebuffer and the
/// palette the same table holds, so a wrong header or a wrong channel order
/// shows up as a byte that differs.
async fn the_ppm_is_the_palette_applied_to_the_frame(admin: &Db, database: &str, frame: &Frame) {
    let source = |column| {
        format!(
            "SELECT {column} FROM {database}.native_frames WHERE frame = {} LIMIT 1",
            frame.frame
        )
    };
    let ppm: bytes::Bytes = admin
        .fetch_one(&ppm_sql_over(
            &source("fb"),
            &source("palette"),
            FB_WIDTH,
            FB_HEIGHT,
        ))
        .await
        .expect("the PPM query");

    let mut want = format!("P6\n{FB_WIDTH} {FB_HEIGHT}\n255\n").into_bytes();
    for pixel in frame.fb.iter() {
        let at = usize::from(*pixel) * 3;
        want.extend_from_slice(&frame.palette[at..at + 3]);
    }
    assert_eq!(ppm.len(), want.len(), "the PPM is not 320x200 RGB triples");
    assert_eq!(
        ppm.as_ref(),
        want.as_slice(),
        "the PPM is not the palette applied to the framebuffer"
    );
}
