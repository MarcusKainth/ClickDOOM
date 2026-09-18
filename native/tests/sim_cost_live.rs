//! What each stage of the tic's first statement costs, on a real
//! ClickHouse server.
//!
//! This is a measurement rather than an assertion about the engine: what it
//! checks is that every cut the generator offers runs against a server and
//! leaves its row, which the structural tests in the crate cannot show
//! because they only read the statement's text. The times it prints are the
//! numbers the benchmarks document quotes, and it prints them for whichever
//! cuts [`CUTS`] names.
//!
//! [`CUTS`] holds a few rather than all of them, because each cut pays its
//! own analysis and the whole set costs minutes. The full ladder is this
//! same helper over `Cut::ALL`, which is one edit here and is how the
//! committed figures were taken.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes it.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::tick::bench::Cut;
use clickdoom_native::{load, sql, wad::Wad};

mod support;

use support::cost;
use support::db::Fixture;

/// The cuts this suite times. The whole of `Cut::ALL` is minutes of
/// analysis, so the committed run uses that and this uses the two ends and
/// the stage they bracket.
const CUTS: [Option<Cut>; 3] = [None, Some(Cut::Project), Some(Cut::Chase)];

/// The tic to time. Far enough in that the level is doing real work, and
/// no further: getting there is a tic of execution each, and every cut
/// after it pays its own analysis, so this suite's cost is roughly one
/// analysis per cut plus this many tics.
const AT_TIC: u32 = 20;

#[tokio::test]
async fn every_cut_runs_the_same_tic_against_the_same_world() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_cost").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(clickdoom_native::sql::sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let costs = cost::at_tic(&fixture, AT_TIC, &CUTS).await;
    let staged: u64 = fixture.count("native_stage").await;
    let at_tic: u64 = fixture
        .scalar(&format!(
            "SELECT count() FROM {db}.native_stage WHERE tic = {AT_TIC}"
        ))
        .await;
    fixture.finish().await;

    for one in &costs {
        println!(
            "tic {AT_TIC} cut {:<8} analysis {:>8.1} ms  one tic {:>8.1} ms  per extra tic {:>7.1} ms",
            one.name(),
            one.analysed.as_secs_f64() * 1000.0,
            one.once.as_secs_f64() * 1000.0,
            one.marginal.as_secs_f64() * 1000.0,
        );
    }

    assert_eq!(costs.len(), CUTS.len(), "every cut was timed");
    // `native_stage` is a `Join(ANY, LEFT, tic)` under
    // `join_any_take_last_row`, so every cut writing the tic replaces the
    // row rather than adding one. What the table shows is that the tic
    // under measurement was reached, not how many cuts reached it.
    assert_eq!(at_tic, 1, "no cut wrote the tic under measurement");
    assert_eq!(
        staged,
        u64::from(AT_TIC),
        "the run to the tic before, plus the tic every cut ran"
    );
    for one in &costs {
        assert!(
            one.once >= one.analysed,
            "cut {}: analysis {:?} exceeds the whole statement {:?}",
            one.name(),
            one.analysed,
            one.once
        );
    }
}
