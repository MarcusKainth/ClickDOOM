//! Seeded arms driven through one resident session, against a real
//! ClickHouse server.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes it.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::tick::Input;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::arms::{Arm, Arms};
use support::db::Fixture;
use support::resident::Session;

/// The last walked tic, which every arm copies.
const BEFORE: u32 = 40;

/// The `leveltime` the marked arm's seed carries.
const MARK: i32 = 5000;

fn arm(name: &'static str, at: u32, overrides: Vec<(&'static str, String)>) -> Arm {
    Arm {
        name,
        from: BEFORE,
        overrides,
        at,
        inputs: (at + 1..=at + 3)
            .map(|tic| Input::keys(tic, 0, (0, 0)))
            .collect(),
    }
}

fn marked() -> Arm {
    arm(
        "marked",
        200,
        vec![("leveltime", format!("toInt32({MARK})"))],
    )
}

fn plain() -> Arm {
    arm("plain", 300, Vec::new())
}

fn walk() -> Vec<Input> {
    (1..=BEFORE).map(Input::demo).collect()
}

#[derive(Row, Deserialize, Debug, PartialEq)]
struct Hashed {
    tic: u32,
    leveltime: i32,
    hash: u64,
}

/// Every column of every row an arm left in either tic table, hashed.
struct Left {
    state: Vec<Hashed>,
    stage: Vec<Hashed>,
}

const HASHED: &str = "tic, toInt32(leveltime) AS leveltime, cityHash64(*) AS hash";

/// Drives `arms` through one session in a database of its own and returns
/// what each arm left, and how many first statements the database saw.
async fn drive(case: &str, arms: Vec<Arm>) -> (Vec<Left>, u64) {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create(case).await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sql::sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let arms = Arms::new(walk(), arms);
    let mut session = Session::open(&fixture, false).await;
    arms.drive(&fixture, &mut session).await;
    session.close().await;

    let mut left = Vec::new();
    for arm in arms.all() {
        left.push(Left {
            state: arm.rows(&fixture, "native_state", HASHED).await,
            stage: arm.rows(&fixture, "native_stage", HASHED).await,
        });
    }
    let analyses = first_statements(&fixture).await;
    fixture.finish().await;
    (left, analyses)
}

#[derive(Row, Deserialize)]
struct Count {
    count: u64,
}

/// How many first statements the server started against `fixture`'s
/// database, each of which paid its own analysis.
async fn first_statements(fixture: &Fixture) -> u64 {
    fixture
        .execute(&[sql::Statement::sql("SYSTEM FLUSH LOGS")])
        .await
        .unwrap();
    fixture
        .scalar::<Count>(&format!(
            "SELECT count() AS count FROM system.query_log \
             WHERE type = 'QueryStart' AND query_kind = 'Insert' \
             AND position(query, 'INSERT INTO {}.native_stage') > 0",
            fixture.database
        ))
        .await
        .count
}

/// Two arms seeded from one walked row in one session leave the same rows,
/// column for column, as each arm driven alone in a database of its own.
/// The marked arm's seed lands while the statements are open, and the tic
/// after it counts on from the seeded `leveltime`.
#[tokio::test]
async fn arms_in_one_session_leave_the_rows_each_leaves_alone() {
    let (both, alone_marked, alone_plain) = tokio::join!(
        drive("session_both", vec![marked(), plain()]),
        drive("session_marked", vec![marked()]),
        drive("session_plain", vec![plain()]),
    );
    let (both, analyses) = both;
    assert_eq!(analyses, 1, "one first statement for the whole session");

    let [marked_left, plain_left] = [&both[0], &both[1]];
    for (left, at) in [(marked_left, marked().at), (plain_left, plain().at)] {
        assert_eq!(left.state.len(), 4, "a seeded row and three tics from it");
        assert_eq!(left.stage.len(), 3, "a stage row for each tic fed");
        assert_eq!(left.state[0].tic, at);
    }
    assert_eq!(marked_left.state[1].leveltime, MARK + 1);
    assert_ne!(
        plain_left.state[1].leveltime,
        MARK + 1,
        "the unmarked arm carries the walk's own leveltime"
    );

    for (left, (alone, analyses)) in [(marked_left, alone_marked), (plain_left, alone_plain)] {
        assert_eq!(analyses, 1, "one first statement for the arm alone");
        assert_eq!(left.state, alone[0].state, "native_state rows");
        assert_eq!(left.stage, alone[0].stage, "native_stage rows");
    }
}

#[test]
#[should_panic(expected = "arm plain on tics 202..=205 meets marked on 200..=203")]
fn arms_that_share_a_tic_are_refused() {
    Arms::new(walk(), vec![marked(), arm("plain", 202, Vec::new())]);
}

#[test]
#[should_panic(expected = "meets the walk on 0..=40")]
fn an_arm_on_the_walk_is_refused() {
    Arms::new(walk(), vec![arm("plain", 30, Vec::new())]);
}

#[test]
#[should_panic(expected = "driven by the demo at tic 301")]
fn an_arm_the_demo_drives_is_refused() {
    let mut demo = plain();
    demo.inputs = (301..=303).map(Input::demo).collect();
    Arms::new(walk(), vec![demo]);
}

#[test]
#[should_panic(expected = "1 of 2 arms checked, 1 failures:\nplain: never checked")]
fn an_arm_never_checked_fails() {
    let arms = Arms::new(walk(), vec![marked(), plain()]);
    let mut checks = arms.checks();
    checks.arm("marked").eq(1, 1, "a check that holds");
    checks.finish();
}

#[test]
#[should_panic(
    expected = "1 of 2 arms checked, 2 failures:\nmarked: leveltime: got 1, want 2\nplain: recorded no check"
)]
fn every_failure_is_reported() {
    let arms = Arms::new(walk(), vec![marked(), plain()]);
    let mut checks = arms.checks();
    checks.arm("marked").eq(1, 2, "leveltime");
    checks.arm("plain");
    checks.finish();
}

#[test]
#[should_panic(expected = "dropped without finish()")]
fn checks_never_finished_panic() {
    let arms = Arms::new(walk(), vec![marked()]);
    let mut checks = arms.checks();
    checks.arm("marked").eq(1, 1, "a check that holds");
}
