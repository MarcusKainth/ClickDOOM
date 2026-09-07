//! `A_Chase`'s own "do not attack twice in a row" branch, against a real
//! ClickHouse server.
//!
//! `demo3` reaches this branch on its own (an imp's chase frame, seven
//! tics after its own fireball throw), which `sim_parity_live` checks
//! against the probe. This seeds the same branch in isolation, so the
//! flag clear, the direction search and the missing melee and missile
//! check are each pinned on their own.
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

/// `states.tsv`: the imp's own chase loop, which carries `A_Chase`. The
/// cycle's own three-tic wait carries the seeded row from the first into
/// the second regardless of which branch `A_Chase` takes.
const CHASE: i32 = 444;
const CHASE_NEXT: i32 = 445;

/// `p_mobj.h`
const MF_SOLID: i32 = 2;
const MF_SHOOTABLE: i32 = 4;
const MF_JUSTATTACKED: i32 = 128;
const MF_COUNTKILL: i32 = 0x400000;

/// `p_enemy.c`: `DI_WEST`, so the target's own north-east offset never
/// lands on the way back.
const MOVEDIR: i32 = 4;

/// The imp's own tic-start position, read once and carried into both the
/// seeded row and the target's own offset from it.
const IMP_X: i32 = 20971520;
const IMP_Y: i32 = -67108864;

/// A player north-east of the imp, comfortably past `CHASE_SLOP` on both
/// axes.
const TARGET_X: i32 = IMP_X + 13107200;
const TARGET_Y: i32 = IMP_Y + 13107200;

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

/// Two slots overridden in the same column. `seed::row` keeps only the
/// first override it finds for a column name, so a column both slots
/// touch needs one combined entry rather than two separate [`put`] calls.
fn put2(
    column: &'static str,
    slot_a: usize,
    value_a: String,
    slot_b: usize,
    value_b: String,
    cast: &str,
) -> (&'static str, String) {
    (
        column,
        format!(
            "arrayMap((v, k) -> {cast}(multiIf(k = {slot_a}, {value_a}, \
             k = {slot_b}, {value_b}, v)), p.{column}, arrayEnumerate(p.{column}))"
        ),
    )
}

#[derive(Row, Deserialize)]
struct Chased {
    #[allow(dead_code)]
    tic: u32,
    state: i32,
    flags: i32,
    movedir: i32,
    x: i32,
    y: i32,
    prndindex: u8,
    unresolved: u64,
    player_health: i32,
}

#[tokio::test]
async fn a_thing_that_just_attacked_does_not_attack_again() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_justattacked").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.extend(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let at = 500;
    let overrides = [
        put("m_state", SUBJECT, CHASE.to_string(), "toInt32"),
        put(
            "m_flags",
            SUBJECT,
            (MF_SOLID | MF_SHOOTABLE | MF_COUNTKILL | MF_JUSTATTACKED).to_string(),
            "toInt32",
        ),
        put("m_tics", SUBJECT, "1".to_owned(), "toInt32"),
        put("m_target", SUBJECT, "1".to_owned(), "toInt32"),
        put("m_movedir", SUBJECT, MOVEDIR.to_string(), "toInt32"),
        put2(
            "m_x",
            SUBJECT,
            IMP_X.to_string(),
            1,
            TARGET_X.to_string(),
            "toInt32",
        ),
        put2(
            "m_y",
            SUBJECT,
            IMP_Y.to_string(),
            1,
            TARGET_Y.to_string(),
            "toInt32",
        ),
    ];
    let mut statements: Vec<sql::Statement> = seed::row(&db, at, BEFORE, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    statements.extend(sim::tick::run_statement(
        &db,
        &[Input::keys(at + 1, 0, (0, 0))],
    ));
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Chased> = fixture
        .rows(&format!(
            "SELECT tic, m_state[{SUBJECT}] AS state, m_flags[{SUBJECT}] AS flags, \
             m_movedir[{SUBJECT}] AS movedir, m_x[{SUBJECT}] AS x, m_y[{SUBJECT}] AS y, \
             prndindex, unresolved, m_health[1] AS player_health \
             FROM {db}.native_state WHERE tic IN ({at}, {}) ORDER BY tic",
            at + 1
        ))
        .await;
    fixture.finish().await;
    assert_eq!(rows.len(), 2, "the seeded row and the tic from it");
    let (before, after) = (&rows[0], &rows[1]);

    assert_eq!(after.unresolved, 0, "the branch this seeds resolves");
    assert_eq!(
        before.flags & MF_JUSTATTACKED,
        MF_JUSTATTACKED,
        "the seeded row carries the mark"
    );
    assert_eq!(
        after.flags & MF_JUSTATTACKED,
        0,
        "the branch clears it before anything else"
    );
    assert_eq!(
        after.state, CHASE_NEXT,
        "the cycle advances to its own next chase frame, not a melee or missile state"
    );
    assert_eq!(
        after.player_health, before.player_health,
        "no melee or missile check runs, so the player takes nothing"
    );
    assert_ne!(
        after.movedir, 8,
        "the target sits open to the north-east, so the search finds a direction"
    );
    assert!(
        after.x != before.x || after.y != before.y,
        "and a direction found is a direction walked"
    );
    assert_eq!(
        after.prndindex,
        before.prndindex.wrapping_add(1),
        "the direct route succeeds first, so the search draws only the fresh move count"
    );
}
