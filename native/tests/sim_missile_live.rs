//! `P_CheckMissileRange`'s early returns, against a real ClickHouse server.
//!
//! The routine draws for the distance only after it has passed the line of
//! sight, the target having just hit the thing, and the reaction time.
//! `demo3` reaches the draw and neither of the other two, so both are
//! seeded into a state row and one tic is run from it.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::sql::sim::tick::Input;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::seed;

/// The last tic the demo drives. Gametic 124 is where the reference run's
/// random-call log records the missile check's draw, so the world it
/// leaves is one the check reaches.
const BEFORE: u32 = 124;

/// `p_mobj.h`: the target has just hit the thing, which makes the check
/// answer yes without drawing. The routine clears the mark as it reads it,
/// and `A_Chase` marks the thing as having attacked.
const MF_JUSTHIT: i32 = 64;
const MF_JUSTATTACKED: i32 = 128;

/// The thing the reference run's random-call log has drawing for its
/// missile check at this tic, which is the one every arm turns on.
const SLOT: usize = 118;

/// One arm per seeded row: where the copy of `BEFORE` lands and which tic
/// runs from it. The tics are far apart so the arms cannot read each
/// other's rows.
const ARMS: [(&str, u32); 3] = [("untouched", 200), ("reactiontime", 300), ("justhit", 400)];

#[derive(Row, Deserialize)]
struct Ran {
    tic: u32,
    prndindex: u8,
    unresolved: u8,
    flags: i32,
}

#[tokio::test]
async fn the_missile_check_draws_only_where_the_engine_draws() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_missile").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    // Each arm is the same world with one column replaced, so the index it
    // leaves differs by exactly the draws the replacement takes away. The
    // command is an empty one built from no keys, so the player draws
    // nothing and only the things move.
    let mut statements: Vec<sql::Statement> = Vec::new();
    for (arm, at) in ARMS {
        let overrides: Vec<(&str, String)> = match arm {
            // `A_Chase` decrements the reaction time before the check
            // reads it, so 2 is the smallest value that stops the draw.
            "reactiontime" => vec![(
                "m_reactiontime",
                "arrayMap(v -> toInt32(2), p.m_reactiontime)".to_owned(),
            )],
            "justhit" => vec![(
                "m_flags",
                format!("arrayMap(v -> toInt32(bitOr(v, {MF_JUSTHIT})), p.m_flags)"),
            )],
            _ => Vec::new(),
        };
        statements.extend(
            seed::row(&db, at, BEFORE, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        statements.push(sim::tick::run_statement(
            &db,
            &[Input::keys(at + 1, 0, (0, 0))],
        ));
    }
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let wanted: Vec<String> = ARMS.iter().map(|(_, at)| (at + 1).to_string()).collect();
    let rows: Vec<Ran> = fixture
        .rows(&format!(
            "SELECT tic, prndindex, unresolved, m_flags[{SLOT}] AS flags \
             FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await;
    let seeded: Vec<Ran> = fixture
        .rows(&format!(
            "SELECT tic, prndindex, unresolved, m_flags[{SLOT}] AS flags \
             FROM {db}.native_state WHERE tic = {BEFORE} ORDER BY tic"
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), ARMS.len(), "every arm ran");
    let start = seeded.first().expect("the row the arms copy").prndindex;
    let at = |arm: &str| {
        let (_, tic) = ARMS.iter().find(|(name, _)| *name == arm).unwrap();
        rows.iter().find(|row| row.tic == tic + 1).unwrap()
    };
    let drew = |arm: &str| at(arm).prndindex.wrapping_sub(start);

    let untouched = drew("untouched");
    assert!(untouched > 0, "the tic draws at all");
    assert_eq!(
        at("untouched").unresolved,
        0,
        "the tic the check answers no on is carried through"
    );
    assert_eq!(
        drew("reactiontime"),
        untouched - 1,
        "a thing still waiting out its reaction time does not draw"
    );
    assert_eq!(at("reactiontime").unresolved, 0, "and the chase carries on");
    // The check answers yes without drawing, and `A_Chase` then puts the
    // thing in its missile frames and returns, so it makes no draw at all
    // where a thing still waiting out its reaction time skips only the
    // check's own number and walks as usual.
    assert!(
        drew("justhit") < drew("reactiontime"),
        "a thing that attacks draws nothing, where one waiting out its \
         reaction time skips one number: {} against {}",
        drew("justhit"),
        drew("reactiontime")
    );
    assert_eq!(
        at("justhit").unresolved,
        0,
        "and the attack it starts is one this runs"
    );
    // `P_CheckMissileRange` clears the mark as it reads it and `A_Chase`
    // marks the thing as having attacked.
    assert_eq!(
        at("justhit").flags & (MF_JUSTHIT | MF_JUSTATTACKED),
        MF_JUSTATTACKED,
        "the mark it answered on is cleared and the attack's own is set"
    );
    assert_eq!(
        at("untouched").flags & (MF_JUSTHIT | MF_JUSTATTACKED),
        0,
        "a thing the check answers no on carries neither"
    );
}
