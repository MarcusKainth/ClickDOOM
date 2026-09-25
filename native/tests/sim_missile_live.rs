//! `P_CheckMissileRange`'s early returns, against a real ClickHouse server.
//!
//! The routine draws for the distance only after it has passed the line of
//! sight, the target having just hit the thing, and the reaction time.
//! `demo3` reaches the draw and neither of the other two, so both are
//! seeded into a state row and one tic is run from it. Every arm runs in
//! one session, so the suite pays the tic statement's analysis once.
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

use support::arms::{Arm, Arms};
use support::db::Fixture;
use support::resident::Session;

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

/// One arm per seeded row: its name and where the copy of `BEFORE` lands.
const ARMS: [(&str, u32); 3] = [("untouched", 200), ("reactiontime", 300), ("justhit", 400)];

#[derive(Row, Deserialize)]
struct Ran {
    tic: u32,
    prndindex: u8,
    unresolved: u64,
    flags: i32,
}

/// Each arm is the same world with one column replaced, so the index it
/// leaves differs by exactly the draws the replacement takes away. The
/// command is an empty one built from no keys, so the player draws nothing
/// and only the things move.
fn arms() -> Arms {
    let arms = ARMS
        .into_iter()
        .map(|(name, at)| {
            let overrides = match name {
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
            Arm {
                name,
                from: BEFORE,
                overrides,
                at,
                inputs: vec![Input::keys(at + 1, 0, (0, 0))],
            }
        })
        .collect();
    Arms::new((1..=BEFORE).map(Input::demo).collect(), arms)
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
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let arms = arms();
    let mut session = Session::open(&fixture, false).await;
    arms.drive(&fixture, &mut session).await;
    session.close().await;

    let columns = format!("tic, prndindex, unresolved, m_flags[{SLOT}] AS flags");
    let mut ran: Vec<Vec<Ran>> = Vec::new();
    for arm in arms.all() {
        ran.push(arm.rows(&fixture, "native_state", &columns).await);
    }
    fixture.finish().await;

    for (arm, rows) in arms.all().iter().zip(&ran) {
        let tics: Vec<u32> = rows.iter().map(|row| row.tic).collect();
        assert_eq!(
            tics,
            Vec::from_iter(arm.tics()),
            "arm {} left its seeded row and the tic run from it",
            arm.name
        );
    }
    // The seeded row carries the walked row's own index, since no arm
    // replaces it.
    let pair = |name: &str| {
        let rows = &ran[arms.all().iter().position(|arm| arm.name == name).unwrap()];
        (&rows[0], &rows[1])
    };
    let at = |name: &str| pair(name).1;
    let drew = |name: &str| {
        let (seeded, after) = pair(name);
        after.prndindex.wrapping_sub(seeded.prndindex)
    };

    let mut checks = arms.checks();
    let untouched = drew("untouched");
    let mut arm = checks.arm("untouched");
    arm.check(untouched > 0, "the tic draws at all");
    arm.eq(
        at("untouched").unresolved,
        0,
        "the tic the check answers no on is carried through",
    );
    arm.eq(
        at("untouched").flags & (MF_JUSTHIT | MF_JUSTATTACKED),
        0,
        "a thing the check answers no on carries neither",
    );

    let mut arm = checks.arm("reactiontime");
    arm.eq(
        drew("reactiontime"),
        untouched.wrapping_sub(1),
        "a thing still waiting out its reaction time does not draw",
    );
    arm.eq(at("reactiontime").unresolved, 0, "and the chase carries on");

    // The check answers yes without drawing, and `A_Chase` then puts the
    // thing in its missile frames and returns, so it makes no draw at all
    // where a thing still waiting out its reaction time skips only the
    // check's own number and walks as usual.
    let mut arm = checks.arm("justhit");
    arm.check(
        drew("justhit") < drew("reactiontime"),
        format_args!(
            "a thing that attacks draws nothing, where one waiting out its \
             reaction time skips one number: {} against {}",
            drew("justhit"),
            drew("reactiontime")
        ),
    );
    arm.eq(
        at("justhit").unresolved,
        0,
        "and the attack it starts is one this runs",
    );
    // `P_CheckMissileRange` clears the mark as it reads it and `A_Chase`
    // marks the thing as having attacked.
    arm.eq(
        at("justhit").flags & (MF_JUSTHIT | MF_JUSTATTACKED),
        MF_JUSTATTACKED,
        "the mark it answered on is cleared and the attack's own is set",
    );
    checks.finish();
}
