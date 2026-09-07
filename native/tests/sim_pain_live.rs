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

use support::db::Fixture;
use support::seed;

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

/// One arm per seeded row: its name and the state it starts from, one tic
/// of wait left on it. The tics are far apart so the arms cannot read each
/// other's rows.
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

#[tokio::test]
async fn a_pain_and_a_death_frame_run_through_a_tic() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_pain").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.extend(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let mut statements: Vec<sql::Statement> = Vec::new();
    for (_, at, start) in ARMS {
        let overrides = [
            put("m_state", SUBJECT, start.to_string(), "toInt32"),
            put("m_tics", SUBJECT, "1".to_owned(), "toInt32"),
        ];
        statements.extend(
            seed::row(&db, at, BEFORE, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        statements.extend(sim::tick::run_statement(
            &db,
            &[Input::keys(at + 1, 0, (0, 0))],
        ));
    }
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let wanted: Vec<String> = ARMS
        .iter()
        .flat_map(|(_, at, _)| [at.to_string(), (at + 1).to_string()])
        .collect();
    let rows: Vec<Cycled> = fixture
        .rows(&format!(
            "SELECT tic, m_state[{SUBJECT}] AS state, m_tics[{SUBJECT}] AS tics, \
             m_flags[{SUBJECT}] AS flags, prndindex, unresolved \
             FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await;
    fixture.finish().await;
    assert_eq!(
        rows.len(),
        ARMS.len() * 2,
        "a seeded row and a tic from it for every arm"
    );
    let at = |tic: u32| {
        rows.iter()
            .find(|row| row.tic == tic)
            .unwrap_or_else(|| panic!("no row for tic {tic}"))
    };

    // `A_Pain` plays a sound and nothing else, so the cycle reaches its
    // frame with the table's own wait and draws no number.
    let (before, after) = (at(200), at(201));
    assert_eq!(
        before.state, PAIN1,
        "the seeded row is a tic from the pain frame"
    );
    assert_eq!(after.state, PAIN2, "and the cycle reaches it");
    assert_eq!(after.tics, 2, "the entered frame's own wait");
    assert_eq!(after.unresolved, 0, "a routine this recognises resolves");
    assert_eq!(after.prndindex, before.prndindex, "A_Pain draws no number");

    // `A_Scream` draws once for the sound: the imp's own death sound is
    // `sfx_bgdth1`, one of the two the switch draws between.
    let (before, after) = (at(300), at(301));
    assert_eq!(
        before.state, DIE1,
        "the seeded row is a tic from the scream"
    );
    assert_eq!(after.state, DIE2, "and the cycle reaches it");
    assert_eq!(after.tics, 8, "the entered frame's own wait");
    assert_eq!(after.unresolved, 0, "a routine this recognises resolves");
    assert_eq!(
        after.prndindex,
        before.prndindex.wrapping_add(1),
        "A_Scream draws once for the sound"
    );

    // `A_Fall` clears the one flag it clears and draws nothing.
    let (before, after) = (at(400), at(401));
    assert_eq!(before.state, DIE3, "the seeded row is a tic from the fall");
    assert_eq!(after.state, DIE4, "and the cycle reaches it");
    assert_eq!(after.tics, 6, "the entered frame's own wait");
    assert_eq!(after.unresolved, 0, "a routine this recognises resolves");
    assert_ne!(
        before.flags & MF_SOLID,
        0,
        "the seeded row still blocks a walk into it"
    );
    assert_eq!(after.flags & MF_SOLID, 0, "and A_Fall takes that off");
    assert_eq!(after.prndindex, before.prndindex, "A_Fall draws no number");
}
