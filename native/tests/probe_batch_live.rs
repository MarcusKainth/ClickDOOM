//! One tic through `run_statement`, for the analysis-cost probe workflow.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, wad::Wad};

mod support;

use support::db::Fixture;

#[tokio::test]
async fn one_batch_tic() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("probe_batch").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.extend(sim::tick::demo_statement(&db, 1, 1));
    let result = fixture.execute(&plan).await;
    fixture.finish().await;
    result.unwrap();
}
