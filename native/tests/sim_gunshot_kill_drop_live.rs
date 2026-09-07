//! The player's own gunshot kill and drop, reached through a tic, against
//! a real ClickHouse server.
//!
//! Before this, `fire_shots`' own `gs_hit` read `hurt::COUNTED` into
//! `now_p_killcount` but never `hurt::DROP`: a zombieman a player's own
//! shot killed lost its clip silently, with no refuse and no drop, unlike
//! the claw and the missile paths this stacks on. This seeds a pistol shot
//! that kills one outright and checks the drop lands the same way theirs
//! do.
//!
//! Seeded from tic 0 rather than run forward from a demo, the same way
//! `sim_refire_live.rs` seeds `A_FirePistol`'s own accuracy: the player
//! stands `STEP` from the target it is moved to face head on, so
//! `psp_accurate`'s own zero spread sends the shot exactly there. Every
//! other mobj is stopped and moved off, so nothing else this tic thinks,
//! shouts or blocks the trace.
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

/// `info.c`: the pistol's own state cycle. `S_PISTOL2` carries
/// `A_FirePistol`.
const S_PISTOL1: i32 = 13;

/// `doomdef.h`
const WP_PISTOL: i32 = 1;
const WP_NOCHANGE: i32 = 10;
/// `d_items.c`: the pistol eats `am_clip`, one-based for `p_ammo`.
const AM_CLIP: usize = 1;

/// The imp turned into the zombieman the shot kills.
const TARGET: usize = 117;

/// `mobjtype.tsv`
const MT_POSSESSED: i32 = 1;
const MT_CLIP: i32 = 63;

/// `mobjinfo.tsv`: `MT_POSSESSED`'s own flags, `MF_SOLID | MF_SHOOTABLE |
/// MF_COUNTKILL`.
const POSSESSED_FLAGS: i64 = 4_194_310;

/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
const MF_CORPSE: i64 = 0x10_0000;
const MF_DROPPED: i64 = 0x2_0000;

/// How far the player stands from the target, due west of it so the
/// accurate shot's own zero spread sends it due east into the target.
const STEP: i64 = 100 * 65536;
/// The angle due east, `R_PointToAngle2`'s own answer for a target that
/// far along the positive x axis.
const DUE_EAST: u32 = 0;

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
async fn a_pistol_shot_that_kills_a_zombieman_counts_it_and_drops_a_clip() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_gunshot_kill_drop").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let at = 200;
    let overrides = [
        ("p_readyweapon", format!("toInt32({WP_PISTOL})")),
        ("p_pendingweapon", format!("toInt32({WP_NOCHANGE})")),
        (
            "psp_state",
            format!("CAST([{S_PISTOL1}, -1], 'Array(Int32)')"),
        ),
        ("psp_tics", "CAST([1, -1], 'Array(Int32)')".to_owned()),
        (
            "p_ammo",
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {AM_CLIP}, 50, v)), \
                 p.p_ammo, arrayEnumerate(p.p_ammo))"
            ),
        ),
        ("p_refire", "toInt32(0)".to_owned()),
        ("p_health", "toInt32(100)".to_owned()),
        ("p_cheats", "toInt32(0)".to_owned()),
        (
            "p_powers",
            "arrayMap(v -> toInt32(0), p.p_powers)".to_owned(),
        ),
        // Every mobj but the player and the target is stopped outright,
        // tics included, and moved off, so nothing else this tic thinks,
        // shouts or stands in the trace's own way.
        (
            "m_state",
            format!(
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo OR k = {TARGET}, v, 0)), \
                 p.m_state, arrayEnumerate(p.m_state))"
            ),
        ),
        (
            "m_tics",
            format!(
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo OR k = {TARGET}, v, -1)), \
                 p.m_tics, arrayEnumerate(p.m_tics))"
            ),
        ),
        (
            "m_x",
            format!(
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo, p.m_x[{TARGET}] - {STEP}, \
                 if(k = {TARGET}, v, -2000000000))), p.m_x, arrayEnumerate(p.m_x))"
            ),
        ),
        (
            "m_y",
            format!(
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo, p.m_y[{TARGET}], v)), \
                 p.m_y, arrayEnumerate(p.m_y))"
            ),
        ),
        (
            "m_z",
            format!(
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo, p.m_z[{TARGET}], v)), \
                 p.m_z, arrayEnumerate(p.m_z))"
            ),
        ),
        (
            "m_angle",
            format!(
                "arrayMap((v, k) -> toUInt32(if(k = p.p_mo, {DUE_EAST}, v)), \
                 p.m_angle, arrayEnumerate(p.m_angle))"
            ),
        ),
        // The target stands in for a zombieman: low health, so the shot's
        // own damage, five times one to three, always kills it.
        put("m_type", TARGET, MT_POSSESSED.to_string(), "toInt32"),
        put("m_flags", TARGET, POSSESSED_FLAGS.to_string(), "toInt32"),
        put("m_health", TARGET, "2".to_owned(), "toInt32"),
        put("m_threshold", TARGET, "0".to_owned(), "toInt32"),
    ];
    let mut statements: Vec<sql::Statement> = seed::row(&db, at, 0, &overrides)
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
    assert!(after.target_health <= 0, "the shot's own damage kills it");
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
        "the player's own gunshot kill counts too"
    );
    assert_eq!(
        after.things,
        before.things + 2,
        "the kill puts the puff and the drop both on the list"
    );
    assert_eq!(after.drop_type, MT_CLIP, "the last thing is a clip");
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
