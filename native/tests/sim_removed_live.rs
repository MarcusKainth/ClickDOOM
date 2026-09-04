//! `P_RemoveMobj` for a thing whose state cycle ran out, against a real
//! ClickHouse server.
//!
//! `demo3` removes the blood a shotgun pellet leaves, which is a run of
//! slots at the end of the list with nothing pointing at them.
//! `sim_parity_live` carries those tics against the engine's own trace.
//! What the demo does not reach is a removal from the middle of the list
//! with pointers either side of it, which is seeded here.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::seed;

/// The tic the run stops on before the seeded row, and the row's own tic.
const BEFORE: u32 = 40;
const SEED_TIC: u32 = 41;

/// The slot put one tic from the end of its cycle. It is in the middle of
/// the list, so every slot above it moves down when it goes.
const GOING: usize = 120;

/// A slot below the one going and one above it, to read the pointers from.
const BELOW: usize = 100;
const ABOVE: usize = 200;

/// A state with nothing after it and no routine, so the tic its wait runs
/// out on is the tic `P_SetMobjState` reaches `S_NULL` from.
const LAST_STATE: i64 = 92;

#[derive(Row, Deserialize)]
struct Listed {
    tic: u32,
    things: u64,
    /// `m_target` of the slot below the one going, which named the slot
    /// above it.
    below_target: u32,
    /// `m_target` of the slot above the one going, which named the one
    /// going, read at the place it stands in this row.
    above_target: u32,
    moved_target: u32,
    /// The `m_x` of the slots either side of the gap, to say the rest of
    /// the list moved down rather than being rewritten.
    below_x: i32,
    above_x: i32,
    moved_x: i32,
    next_seq: u32,
    unresolved: u8,
}

async fn run() -> (Listed, Listed) {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_removed").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let put = |column: &'static str, at: usize, value: String| {
        (
            column,
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {at}, {value}, v)), \
                 p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    };
    let point = |at: usize, value: usize| {
        (
            "m_target",
            format!(
                "arrayMap((v, k) -> toUInt32(multiIf(k = {BELOW}, {ABOVE}, \
                 k = {ABOVE}, {GOING}, k = {at}, {value}, v)), \
                 p.m_target, arrayEnumerate(p.m_target))"
            ),
        )
    };
    let overrides = [
        // One tic left on a state that leads to `S_NULL`.
        put("m_state", GOING, LAST_STATE.to_string()),
        put("m_tics", GOING, "1".to_owned()),
        // The slot below the gap names one above it, and the slot above
        // the gap names the thing that is about to go.
        point(GOING, GOING),
    ];
    let seeded: Vec<sql::Statement> = seed::row(&db, SEED_TIC, BEFORE, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let tic = sim::tick::demo_statement(&db, SEED_TIC + 1, SEED_TIC + 1);
    if let Err(error) = fixture.execute(&[tic]).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let rows: Vec<Listed> = fixture
        .rows(&format!(
            "SELECT tic, toUInt64(length(m_x)) AS things, \
             m_target[{BELOW}] AS below_target, m_target[{ABOVE}] AS above_target, \
             m_target[{ABOVE} - 1] AS moved_target, m_x[{BELOW}] AS below_x, \
             m_x[{ABOVE}] AS above_x, m_x[{ABOVE} - 1] AS moved_x, next_seq, unresolved \
             FROM {db}.native_state WHERE tic IN ({SEED_TIC}, {}) ORDER BY tic",
            SEED_TIC + 1
        ))
        .await;
    fixture.finish().await;
    assert_eq!(rows.len(), 2, "the seeded row and the tic after it");
    let mut rows = rows.into_iter();
    (rows.next().unwrap(), rows.next().unwrap())
}

#[tokio::test]
async fn a_thing_taken_from_the_middle_moves_every_slot_above_it_down() {
    let (before, after) = run().await;
    assert_eq!(before.tic, SEED_TIC);
    assert_eq!(
        (before.below_target, before.above_target),
        (ABOVE as u32, GOING as u32),
        "the seeded row points across the slot that is about to go"
    );
    assert_eq!(after.unresolved, 0, "the tic runs");
    assert_eq!(
        after.things,
        before.things - 1,
        "one thing came off the list"
    );
    // The slot the pointer named was above the gap, so it moved down by
    // one and the pointer moved with it.
    assert_eq!(
        after.below_target,
        ABOVE as u32 - 1,
        "a pointer at a slot above the gap follows it down"
    );
    // The thing at that slot is the same thing, one place lower.
    assert_eq!(
        after.moved_x, before.above_x,
        "the slot the pointer now names holds what it named before"
    );
    assert_eq!(
        after.below_x, before.below_x,
        "a slot below the gap does not move"
    );
    // The pointer that named the thing taken has nothing to name.
    assert_eq!(
        after.moved_target, 0,
        "a pointer at the thing that went reads as none"
    );
    assert_eq!(
        after.next_seq, before.next_seq,
        "what the level has ever spawned does not count down"
    );
}
