//! Live proof for `clickdoom native demo`, run as a caller runs it.
//!
//! The binary, not the library: what this covers is the command line and
//! what a run leaves behind, over a database the same binary loaded.
//!
//!   * `--stop-at-frame` stops there, and the run draws every frame up to
//!     it;
//!   * `--hash-out` names each frame, the tic it was drawn from and its
//!     hash, and each hash is the one the engine's own frame had;
//!   * `--frame-dir` leaves one PPM per frame, named by the frame;
//!   * `--expect-probe-fbhash` exits 3 on a frame the engine did not draw,
//!     which is what `native/tests/fixtures/README.md` shows for a frame
//!     rendered over the wrong one.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`) and the committed
//! probe fixture. One run of the demo, because a renderer statement holds
//! its scalar constants for as long as it is open.

#![cfg(feature = "clickhouse-tests")]

use std::process::Command;

mod support;

use support::{committed_fixture, conn_args, repo_root};

/// The last frame the committed fixture can draw: it holds the melt's first
/// and last frames and the two after them, then jumps, and a frame draws
/// over the one before it.
const LAST_FRAME: u32 = 41;

fn clickdoom(database: &str, args: &[&str]) -> (i32, String) {
    let conn = conn_args(database);
    let output = Command::new(env!("CARGO_BIN_EXE_clickdoom"))
        .current_dir(repo_root())
        .args(args)
        .args([
            "--host",
            &conn.host,
            "--port",
            &conn.port.to_string(),
            "--database",
            database,
            "--password",
            &conn.resolved_password(),
        ])
        .output()
        .unwrap_or_else(|e| panic!("running clickdoom {args:?}: {e}"));
    let mut printed = String::from_utf8_lossy(&output.stdout).into_owned();
    printed.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), printed)
}

#[tokio::test]
async fn a_demo_run_draws_every_frame_and_leaves_what_it_was_asked_to() {
    let database = format!("clickdoom_native_demo_{}", std::process::id());
    let out = std::env::temp_dir().join(&database);
    let frames = out.join("frames");
    let hashes = out.join("hashes.tsv");
    std::fs::create_dir_all(&out).expect("a temporary directory");

    let (code, printed) = clickdoom(&database, &["native", "load", "--fresh"]);
    assert_eq!(code, 0, "{printed}");
    let fixture = committed_fixture();
    let (code, printed) = clickdoom(
        &database,
        &[
            "native",
            "load",
            "--probe",
            fixture.to_str().expect("a path"),
        ],
    );
    assert_eq!(code, 0, "{printed}");

    let (code, printed) = clickdoom(
        &database,
        &[
            "native",
            "demo",
            "demo3",
            "--no-window",
            "--stop-at-frame",
            &LAST_FRAME.to_string(),
            "--frame-dir",
            frames.to_str().expect("a path"),
            "--hash-out",
            hashes.to_str().expect("a path"),
            "--expect-probe-fbhash",
        ],
    );
    assert_eq!(code, 0, "{printed}");
    assert!(printed.contains("# native final "), "{printed}");

    // Every frame the fixture holds up to the one asked for, with the tic
    // it was drawn from and the hash the engine's own frame had. The
    // command checked the hashes as it drew, so this is the file's shape.
    let written = std::fs::read_to_string(&hashes).expect("the hash file");
    let mut lines = written.lines();
    assert_eq!(lines.next(), Some("frame\ttic\tfb_hash"));
    let rows: Vec<Vec<&str>> = lines.map(|line| line.split('\t').collect()).collect();
    assert_eq!(
        rows.iter().map(|r| r[0]).collect::<Vec<_>>(),
        ["0", "39", "40", "41"],
        "the run drew something other than the fixture's frames"
    );
    assert_eq!(rows[2], ["40", "3", "2eb87849ee6d9714"]);
    assert!(
        rows.iter().all(|r| r[2].len() == 16),
        "a hash is not 16 hex digits"
    );

    // One PPM per frame, named by the frame, and 320x200 RGB triples behind
    // the header the format needs.
    for row in &rows {
        let frame: u32 = row[0].parse().expect("a frame number");
        let path = frames.join(format!("frame-{frame:05}.ppm"));
        let ppm = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(ppm.starts_with(b"P6\n320 200\n255\n"), "frame {frame}");
        assert_eq!(ppm.len(), 15 + 320 * 200 * 3, "frame {frame}");
    }

    // Frame 1000 draws over frame 999, which the fixture does not hold, so
    // asking for it is refused before anything is rendered.
    let (code, printed) = clickdoom(
        &database,
        &["native", "demo", "--no-window", "--stop-at-frame", "1000"],
    );
    assert_eq!(code, 1, "{printed}");
    assert!(printed.contains("frame 999"), "{printed}");

    std::fs::remove_dir_all(&out).ok();
    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}
