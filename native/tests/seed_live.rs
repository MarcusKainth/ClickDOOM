//! Seeded state rows against a real ClickHouse server.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes it.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::seed;

#[derive(Row, Deserialize)]
struct Leveltime {
    leveltime: i32,
}

async fn leveltime(fixture: &Fixture, tic: u32) -> i32 {
    fixture
        .scalar::<Leveltime>(&format!(
            "SELECT toInt32(leveltime) AS leveltime FROM {}.native_state WHERE tic = {tic}",
            fixture.database
        ))
        .await
        .leveltime
}

/// Seeds tic 5 from tic 0 with `leveltime` replaced, then tic 6 from tic 5
/// with nothing replaced, in one database. Tic 6 carries tic 5's
/// `leveltime`, so the second seed read its own source row.
#[tokio::test]
async fn each_seed_copies_its_own_source_row() {
    const MARK: i32 = 500;

    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("seed").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sql::sim::load_statements(&db));
    plan.extend(
        seed::row(&db, 5, 0, &[("leveltime", format!("toInt32({MARK})"))])
            .into_iter()
            .chain(seed::row(&db, 6, 5, &[]))
            .map(sql::Statement::sql),
    );
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let at = [
        leveltime(&fixture, 0).await,
        leveltime(&fixture, 5).await,
        leveltime(&fixture, 6).await,
    ];
    fixture.finish().await;

    assert_ne!(
        at[0], MARK,
        "tic 0 already holds the mark, so the check is vacuous"
    );
    assert_eq!(at[1], MARK, "the seed at tic 5 carries its override");
    assert_eq!(
        at[2], MARK,
        "the seed at tic 6 copies tic 5, its own source, not tic 0"
    );
}
