//! An in-flight missile's own kill and drop, reached through a tic,
//! against a real ClickHouse server.
//!
//! A fireball already on the list, aimed to land exactly on a target one
//! tic away, follows `sim_missile_live.rs`'s own seeding shape. The
//! target's own type and flags are overridden to a zombieman's, the same
//! way `sim_kills_and_drops_live.rs`'s claw arm does, so the impact's own
//! damage kills it.
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

/// The imp turned into an in-flight fireball, and the imp turned into the
/// zombieman it lands on.
const MISSILE: usize = 116;
const TARGET: usize = 117;

/// `mobjtype.tsv`
const MT_TROOPSHOT: i32 = 31;
const MT_POSSESSED: i32 = 1;
const MT_CLIP: i32 = 63;

/// `mobjinfo.tsv`: `MT_TROOPSHOT`'s own flags, `MF_MISSILE | MF_DROPOFF |
/// MF_NOGRAVITY | MF_NOBLOCKMAP`; `MT_POSSESSED`'s own, `MF_SOLID |
/// MF_SHOOTABLE | MF_COUNTKILL`.
const TROOPSHOT_FLAGS: i64 = 67_088;
const POSSESSED_FLAGS: i64 = 4_194_310;
/// `mobjinfo.tsv`: `MT_TROOPSHOT`'s own radius and height.
const TROOPSHOT_RADIUS: i64 = 393_216;
const TROOPSHOT_HEIGHT: i64 = 524_288;
/// `states.tsv`: `MT_TROOPSHOT`'s own spawnstate, and its own wait.
const TROOPSHOT_SPAWNSTATE: i32 = 97;
const TROOPSHOT_SPAWNTICS: i32 = 4;

/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
const MF_CORPSE: i64 = 0x10_0000;
const MF_DROPPED: i64 = 0x2_0000;

/// How far the missile starts from its target and moves in the one tic
/// this runs, landing exactly on it.
const STEP: i64 = 5 * 65536;

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
struct Kill {
    tic: u32,
    target_health: i32,
    target_flags: i32,
    target_x: i32,
    target_y: i32,
    target_z: i32,
    killcount: i32,
    unresolved: u64,
    things: u64,
    drop_type: i32,
    drop_flags: i32,
    drop_x: i32,
    drop_y: i32,
    drop_z: i32,
}

#[tokio::test]
async fn an_in_flight_fireball_that_kills_a_zombieman_counts_it_and_drops_a_clip() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_missile_kill_drop").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let at = BEFORE + 100;
    let overrides = [
        // The target stands in for a zombieman: low health, so the
        // fireball's own damage, one to eight times three, always kills
        // it. The missile stands in for a fireball; both slots move in
        // `m_type`, `m_flags` and `m_threshold`, so those three go through
        // `put2` rather than colliding as two `put` entries under the
        // same column name.
        put2(
            "m_type",
            TARGET,
            MT_POSSESSED.to_string(),
            MISSILE,
            MT_TROOPSHOT.to_string(),
            "toInt32",
        ),
        put2(
            "m_flags",
            TARGET,
            POSSESSED_FLAGS.to_string(),
            MISSILE,
            TROOPSHOT_FLAGS.to_string(),
            "toInt32",
        ),
        put2(
            "m_threshold",
            TARGET,
            "0".to_owned(),
            MISSILE,
            "0".to_owned(),
            "toInt32",
        ),
        put("m_health", TARGET, "2".to_owned(), "toInt32"),
        // The missile starts `STEP` west of the target and moves exactly
        // `STEP` east, landing on it.
        put(
            "m_x",
            MISSILE,
            format!("p.m_x[{TARGET}] - {STEP}"),
            "toInt32",
        ),
        put("m_y", MISSILE, format!("p.m_y[{TARGET}]"), "toInt32"),
        put("m_z", MISSILE, format!("p.m_z[{TARGET}]"), "toInt32"),
        put(
            "m_floorz",
            MISSILE,
            format!("p.m_floorz[{TARGET}]"),
            "toInt32",
        ),
        put(
            "m_ceilingz",
            MISSILE,
            format!("p.m_ceilingz[{TARGET}]"),
            "toInt32",
        ),
        put(
            "m_subsector",
            MISSILE,
            format!("p.m_subsector[{TARGET}]"),
            "toInt32",
        ),
        put("m_momx", MISSILE, STEP.to_string(), "toInt32"),
        put("m_momy", MISSILE, "0".to_owned(), "toInt32"),
        put("m_momz", MISSILE, "0".to_owned(), "toInt32"),
        put("m_radius", MISSILE, TROOPSHOT_RADIUS.to_string(), "toInt32"),
        put("m_height", MISSILE, TROOPSHOT_HEIGHT.to_string(), "toInt32"),
        put(
            "m_state",
            MISSILE,
            TROOPSHOT_SPAWNSTATE.to_string(),
            "toInt32",
        ),
        put(
            "m_tics",
            MISSILE,
            TROOPSHOT_SPAWNTICS.to_string(),
            "toInt32",
        ),
        put("m_target", MISSILE, "0".to_owned(), "toUInt32"),
    ];
    let mut statements: Vec<sql::Statement> = seed::row(&db, at, BEFORE, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    statements.push(sim::tick::run_statement(
        &db,
        &[Input::keys(at + 1, 0, (0, 0))],
    ));
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Kill> = fixture
        .rows(&format!(
            "SELECT tic, m_health[{TARGET}] AS target_health, \
             m_flags[{TARGET}] AS target_flags, m_x[{TARGET}] AS target_x, \
             m_y[{TARGET}] AS target_y, m_z[{TARGET}] AS target_z, \
             p_killcount AS killcount, unresolved, \
             toUInt64(length(m_x)) AS things, m_type[length(m_type)] AS drop_type, \
             m_flags[length(m_flags)] AS drop_flags, m_x[length(m_x)] AS drop_x, \
             m_y[length(m_y)] AS drop_y, m_z[length(m_z)] AS drop_z \
             FROM {db}.native_state WHERE tic IN ({at}, {}) ORDER BY tic",
            at + 1
        ))
        .await;
    fixture.finish().await;
    assert_eq!(rows.len(), 2, "the seeded row and the tic run from it");
    let before = &rows[0];
    let after = &rows[1];
    assert_eq!(before.tic, at, "the seeded row comes first");
    assert_eq!(after.tic, at + 1, "and the tic it ran comes second");

    assert_eq!(after.unresolved, 0, "the kill and its drop both resolve");
    assert!(
        after.target_health <= 0,
        "the fireball's own damage kills it"
    );
    assert_eq!(
        after.target_flags & MF_SHOOTABLE as i32,
        0,
        "the corpse is no longer shootable"
    );
    assert_eq!(
        after.target_flags & MF_CORPSE as i32,
        MF_CORPSE as i32,
        "and carries the corpse flag"
    );
    assert_eq!(
        after.killcount,
        before.killcount + 1,
        "an in-flight missile's own kill still counts"
    );
    assert_eq!(
        after.things,
        before.things + 1,
        "the kill puts one more thing on the list"
    );
    assert_eq!(after.drop_type, MT_CLIP, "and it is a clip");
    assert_eq!(
        after.drop_flags & MF_DROPPED as i32,
        MF_DROPPED as i32,
        "marked as dropped"
    );
    assert_eq!(
        (after.drop_x, after.drop_y),
        (before.target_x, before.target_y),
        "spawned where the corpse lies"
    );
    assert_eq!(
        after.drop_z, before.target_z,
        "on the floor beneath the corpse, which already rests on it"
    );
}
