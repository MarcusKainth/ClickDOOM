//! `A_Pain`, `A_Scream` and `A_Fall` reached through a tic, against a real
//! ClickHouse server.
//!
//! `demo3` never puts a monster through its pain or death frames, so every
//! arm is seeded: an imp already on the level's own list, one tic from the
//! frame the routine sits on.
//!
//! Every arm is a row seeded into one session, because a session pays the
//! tic statement's analysis once.
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

/// The tic every arm copies its row from. Gametic 40 is early enough that
/// no monster has woken and the list still holds the level's own things.
const BEFORE: u32 = 40;

/// An imp on the level's own list, `MT_TROOP`.
const SUBJECT: usize = 116;

/// `states.tsv`: the imp's own pain chain, `S_TROO_PAIN` into
/// `S_TROO_PAIN2`, which carries `A_Pain`.
const PAIN1: i32 = 455;
const PAIN2: i32 = 456;

/// `states.tsv`: the imp's own death chain. `S_TROO_DIE2` carries
/// `A_Scream` and `S_TROO_DIE4` carries `A_Fall`.
const DIE1: i32 = 457;
const DIE2: i32 = 458;
const DIE3: i32 = 459;
const DIE4: i32 = 460;

/// `p_mobj.h`
const MF_SOLID: i32 = 2;

/// One arm per seeded row: its name, where the copy of `BEFORE` lands and
/// the state it starts from, one tic of wait left on it.
const ARMS: [(&str, u32, i32); 3] = [
    ("pain", 200, PAIN1),
    ("scream", 300, DIE1),
    ("falls", 400, DIE3),
];

#[derive(Row, Deserialize)]
struct Cycled {
    tic: u32,
    state: i32,
    tics: i32,
    flags: i32,
    prndindex: u8,
    unresolved: u64,
}

/// A column of one slot replaced, leaving every other slot alone.
fn put(column: &'static str, slot: usize, value: String, cast: &str) -> (&'static str, String) {
    (
        column,
        format!(
            "arrayMap((v, k) -> {cast}(if(k = {slot}, {value}, v)), \
             p.{column}, arrayEnumerate(p.{column}))"
        ),
    )
}

fn arms() -> Arms {
    let arms = ARMS
        .into_iter()
        .map(|(name, at, start)| Arm {
            name,
            from: BEFORE,
            overrides: vec![
                put("m_state", SUBJECT, start.to_string(), "toInt32"),
                put("m_tics", SUBJECT, "1".to_owned(), "toInt32"),
            ],
            at,
            inputs: vec![Input::keys(at + 1, 0, (0, 0))],
        })
        .collect();
    Arms::new((1..=BEFORE).map(Input::demo).collect(), arms)
}

#[tokio::test]
async fn a_pain_and_a_death_frame_run_through_a_tic() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_pain").await;
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

    let columns = format!(
        "tic, m_state[{SUBJECT}] AS state, m_tics[{SUBJECT}] AS tics, \
         m_flags[{SUBJECT}] AS flags, prndindex, unresolved"
    );
    let mut ran: Vec<Vec<Cycled>> = Vec::new();
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
    let pair = |name: &str| {
        let rows = &ran[arms.all().iter().position(|arm| arm.name == name).unwrap()];
        (&rows[0], &rows[1])
    };
    let mut checks = arms.checks();

    // `A_Pain` plays a sound and nothing else, so the cycle reaches its
    // frame with the table's own wait and draws no number.
    let (before, after) = pair("pain");
    let mut arm = checks.arm("pain");
    arm.eq(
        before.state,
        PAIN1,
        "the seeded row is a tic from the pain frame",
    );
    arm.eq(after.state, PAIN2, "and the cycle reaches it");
    arm.eq(after.tics, 2, "the entered frame's own wait");
    arm.eq(after.unresolved, 0, "a routine this recognises resolves");
    arm.eq(after.prndindex, before.prndindex, "A_Pain draws no number");

    // `A_Scream` draws once for the sound: the imp's own death sound is
    // `sfx_bgdth1`, one of the two the switch draws between.
    let (before, after) = pair("scream");
    let mut arm = checks.arm("scream");
    arm.eq(
        before.state,
        DIE1,
        "the seeded row is a tic from the scream",
    );
    arm.eq(after.state, DIE2, "and the cycle reaches it");
    arm.eq(after.tics, 8, "the entered frame's own wait");
    arm.eq(after.unresolved, 0, "a routine this recognises resolves");
    arm.eq(
        after.prndindex,
        before.prndindex.wrapping_add(1),
        "A_Scream draws once for the sound",
    );

    // `A_Fall` clears the one flag it clears and draws nothing.
    let (before, after) = pair("falls");
    let mut arm = checks.arm("falls");
    arm.eq(before.state, DIE3, "the seeded row is a tic from the fall");
    arm.eq(after.state, DIE4, "and the cycle reaches it");
    arm.eq(after.tics, 6, "the entered frame's own wait");
    arm.eq(after.unresolved, 0, "a routine this recognises resolves");
    arm.check(
        before.flags & MF_SOLID != 0,
        "the seeded row still blocks a walk into it",
    );
    arm.eq(after.flags & MF_SOLID, 0, "and A_Fall takes that off");
    arm.eq(after.prndindex, before.prndindex, "A_Fall draws no number");
    checks.finish();
}
