//! Live proof for `clickdoom native diff`, run as a caller runs it.
//!
//!   * a run whose probe records none of the tics it ran says so and fails,
//!     rather than reporting agreement over nothing;
//!   * a run over tics the probe does record reports the first field that
//!     differs, with the tic and both values, and exits 3;
//!   * the probe rows land in `probe_state` and not in `native_state`,
//!     because the two are the sides of the comparison;
//!   * a tic `native_state` marks unresolved stops the run there, before
//!     any field is compared.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`) and the committed
//! probe fixture.

#![cfg(feature = "clickhouse-tests")]

use std::path::{Path, PathBuf};
use std::process::Command;

mod support;

use support::{committed_fixture, conn_args, repo_root};

/// The gametics the committed fixture records: the melt's, the first
/// gameplay tic, the one after it, and one from the middle of the demo.
const FIRST_RECORDED_TIC: u32 = 2;

/// The first tic DEMO3 on E1M7 leaves unresolved, read off a real run: a
/// thing's state cycle enters a frame carrying a routine `entry_one`
/// does not recognise. A fix that closes the gap behind it moves this
/// later; update it in the same commit as that fix.
const FIRST_REFUSED_TIC: u32 = 144;

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
async fn a_differential_run_reports_the_first_field_that_differs() {
    let database = format!("clickdoom_native_diff_{}", std::process::id());
    let (code, printed) = clickdoom(&database, &["native", "load", "--fresh"]);
    assert_eq!(code, 0, "{printed}");

    let fixture = committed_fixture();
    let probe = fixture.to_str().expect("a path");

    // The fixture's earliest gametic is 2, so a run of one tic compares
    // nothing. Reporting agreement there would be a check that never ran.
    let short = (FIRST_RECORDED_TIC - 1).to_string();
    let (code, printed) = clickdoom(&database, &["native", "diff", &short, "--probe", probe]);
    assert_eq!(code, 1, "{printed}");
    assert!(printed.contains("nothing was compared"), "{printed}");

    // Over the tics the fixture records and the simulation reproduces, the
    // two sides agree.
    let tics = FIRST_RECORDED_TIC.to_string();
    let (code, printed) = clickdoom(&database, &["native", "diff", &tics, "--probe", probe]);
    assert_eq!(code, 0, "{printed}");
    assert!(printed.contains("no divergence"), "{printed}");

    // A probe that differs from the simulation on one field is reported on
    // the tic and the field, with exit 3. The fixture is copied with one
    // value moved, so the comparison has something to find whatever the
    // simulation reproduces.
    let moved = moved_fixture(&fixture, FIRST_RECORDED_TIC, "leveltime");
    let moved_probe = moved.to_str().expect("a path");
    let (code, printed) = clickdoom(
        &database,
        &["native", "diff", &tics, "--probe", moved_probe],
    );
    assert_eq!(code, 3, "{printed}");
    assert!(
        printed.contains(&format!("tic {FIRST_RECORDED_TIC} ")),
        "{printed}"
    );
    assert!(printed.contains("leveltime"), "{printed}");
    assert!(printed.contains("against the probe's"), "{printed}");

    // The two sides are two tables. A diff that copied the probe into
    // native_state would compare the run against itself and always agree.
    let db = conn_args(&database).connect();
    let staged: u64 = db
        .fetch_one(&format!("SELECT count() FROM {database}.probe_state"))
        .await
        .expect("the staging table");
    assert!(staged > 0, "the probe rows did not land");
    let ours: u64 = db
        .fetch_one(&format!("SELECT count() FROM {database}.native_state"))
        .await
        .expect("the state table");
    assert_eq!(
        ours,
        u64::from(FIRST_RECORDED_TIC) + 1,
        "native_state holds the run's own tics and the level's first row, and nothing else"
    );

    let (code, printed) = clickdoom(
        &database,
        &["native", "diff", &tics, "--probe", moved_probe, "--summary"],
    );
    assert_eq!(code, 3, "{printed}");
    assert!(printed.contains("first_tic="), "{printed}");
    std::fs::remove_file(&moved).ok();

    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}

/// A tic `native_state` marks unresolved stops the run there, with exit 3
/// and a message naming the tic and the column, before any field is
/// compared against the probe.
///
/// The committed fixture covers only a handful of gametics, far short of
/// `FIRST_REFUSED_TIC`, but the refusal is checked before the comparison
/// needs the probe to cover anything, so it does not need a fuller one.
#[tokio::test]
async fn a_tic_that_refuses_stops_before_the_field_comparison() {
    let database = format!("clickdoom_native_diff_refusal_{}", std::process::id());
    let (code, printed) = clickdoom(&database, &["native", "load", "--fresh"]);
    assert_eq!(code, 0, "{printed}");

    let fixture = committed_fixture();
    let probe = fixture.to_str().expect("a path");
    let tics = (FIRST_REFUSED_TIC + 8).to_string();
    let (code, printed) = clickdoom(&database, &["native", "diff", &tics, "--probe", probe]);
    assert_eq!(code, 3, "{printed}");
    assert!(
        printed.contains(&format!("tic {FIRST_REFUSED_TIC} unresolved")),
        "{printed}"
    );
    assert!(
        !printed.contains("no divergence") && !printed.contains("against the probe's"),
        "a refused tic is reported before any field is compared: {printed}"
    );

    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}

/// A copy of `fixture` with `column` moved by one on every row of
/// `gametic`, written beside the temporary files, so a differential has one
/// field to report. Every row, because the melt commits several frames in
/// one gametic and the comparison reads the last of them.
fn moved_fixture(fixture: &Path, gametic: u32, column: &str) -> PathBuf {
    let text = std::fs::read_to_string(fixture).expect("the fixture is readable");
    let header = text
        .lines()
        .find(|line| line.starts_with("# columns"))
        .expect("the fixture names its columns");
    let names: Vec<&str> = header.split('\t').skip(1).collect();
    let at = names
        .iter()
        .position(|name| *name == column)
        .unwrap_or_else(|| panic!("the fixture carries no {column} column"));
    let gametic_at = names
        .iter()
        .position(|name| *name == "gametic")
        .expect("the fixture carries a gametic column");
    let mut moved_one = false;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            if line.starts_with('#') {
                return line.to_owned();
            }
            let mut fields: Vec<String> = line.split('\t').map(str::to_owned).collect();
            if fields.get(gametic_at).and_then(|g| g.parse::<u32>().ok()) != Some(gametic) {
                return line.to_owned();
            }
            let value: i64 = fields[at].parse().expect("the moved column is a number");
            fields[at] = (value + 1).to_string();
            moved_one = true;
            fields.join("\t")
        })
        .collect();
    assert!(moved_one, "no row of gametic {gametic} in the fixture");
    let path = std::env::temp_dir().join(format!(
        "clickdoom-native-diff-moved-{}.tsv",
        std::process::id()
    ));
    std::fs::write(&path, lines.join("\n") + "\n").expect("the moved fixture is written");
    path
}
