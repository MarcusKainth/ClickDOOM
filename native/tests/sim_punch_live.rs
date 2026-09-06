//! `A_Punch`, on a real ClickHouse server.
//!
//! Seeded straight from the level's own first row: the player faces due
//! east with a fixed `prndindex`, so the damage roll is exact, and a
//! second mobj is moved into melee range along that same line while every
//! other mobj is moved out of it, so the punch can only ever reach the one
//! thing this placed there or nothing at all.
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

use support::damage::point_to_angle;
use support::db::Fixture;
use support::seed;

/// `info.c`: the fist's own state cycle. `S_PUNCH1` carries no action and
/// its `nextstate` is `S_PUNCH2`, where `A_Punch` runs.
const S_PUNCH1: i32 = 5;
const S_PUNCH2: i32 = 6;

/// `doomdef.h`
const WP_FIST: i32 = 0;
const WP_NOCHANGE: i32 = 10;
/// `d_player.h`: `pw_strength`, one-based for `p_powers`.
const PW_STRENGTH: usize = 2;
/// `p_mobj.h`
const MF_SOLID: i64 = 2;
const MF_SHOOTABLE: i64 = 4;
/// `p_local.h`'s `MELEERANGE` is `64 * FRACUNIT` (4,194,304); comfortably
/// inside it, and far enough off the axis that a hit turns the player by
/// a real angle.
const TARGET_X: i64 = 2_000_000;
const TARGET_Y: i64 = 500_000;
/// Moved far enough off that neither the aim nor the attack can reach it.
const FAR_AWAY: i64 = -2_000_000_000;
/// The health the target starts with, high enough that neither arm's roll
/// can kill it, which would take the test down `P_KillMobj`'s own path.
const TARGET_HEALTH: i64 = 1000;

#[derive(Row, Deserialize)]
struct Punched {
    tic: u32,
    psp_state: Vec<i32>,
    m_angle: Vec<u32>,
    m_health: Vec<i32>,
    unresolved: u64,
    p_mo: u32,
}

/// One arm: its name, where the copy of tic 0 lands, whether the target is
/// placed in range, and whether the punch is thrown with berserk strength.
struct Arm {
    name: &'static str,
    at: u32,
    in_range: bool,
    berserk: bool,
}

#[tokio::test]
async fn a_punch_lands_only_where_the_engine_lands_it() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_punch").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let arms = [
        Arm {
            name: "connects",
            at: 200,
            in_range: true,
            berserk: false,
        },
        Arm {
            name: "connects_berserk",
            at: 300,
            in_range: true,
            berserk: true,
        },
        Arm {
            name: "misses",
            at: 400,
            in_range: false,
            berserk: false,
        },
    ];
    let mut statements: Vec<sql::Statement> = Vec::new();
    for arm in &arms {
        let target_x = if arm.in_range { TARGET_X } else { FAR_AWAY };
        let target_y = if arm.in_range { TARGET_Y } else { FAR_AWAY };
        let powers = if arm.berserk { 1 } else { 0 };
        let overrides: Vec<(&str, String)> = vec![
            ("p_readyweapon", format!("toInt32({WP_FIST})")),
            ("p_pendingweapon", format!("toInt32({WP_NOCHANGE})")),
            (
                "psp_state",
                format!("CAST([{S_PUNCH1}, -1], 'Array(Int32)')"),
            ),
            ("psp_tics", "CAST([1, -1], 'Array(Int32)')".to_owned()),
            ("prndindex", "toUInt8(0)".to_owned()),
            (
                "p_powers",
                format!(
                    "arrayMap((v, k) -> toInt32(if(k = {PW_STRENGTH}, {powers}, v)), \
                     p.p_powers, arrayEnumerate(p.p_powers))"
                ),
            ),
            ("p_health", "toInt32(100)".to_owned()),
            // The player faces due east, and everything but the target
            // moves off that line so the punch can only ever reach it or
            // nothing.
            (
                "m_angle",
                "arrayMap((v, k) -> toUInt32(if(k = p.p_mo, 0, v)), \
                 p.m_angle, arrayEnumerate(p.m_angle))"
                    .to_owned(),
            ),
            (
                "m_x",
                format!(
                    "arrayMap((v, k) -> toInt32(if(k = p.p_mo, v, \
                     if(k = ((p.p_mo % length(p.m_state)) + 1), \
                     toInt32(p.m_x[p.p_mo]) + {target_x}, {FAR_AWAY}))), \
                     p.m_x, arrayEnumerate(p.m_x))"
                ),
            ),
            (
                "m_y",
                format!(
                    "arrayMap((v, k) -> toInt32(if(k = p.p_mo, v, \
                     if(k = ((p.p_mo % length(p.m_state)) + 1), \
                     toInt32(p.m_y[p.p_mo]) + {target_y}, {FAR_AWAY}))), \
                     p.m_y, arrayEnumerate(p.m_y))"
                ),
            ),
            (
                "m_z",
                "arrayMap((v, k) -> toInt32(if(k = ((p.p_mo % length(p.m_state)) + 1), \
                 p.m_z[p.p_mo], v)), p.m_z, arrayEnumerate(p.m_z))"
                    .to_owned(),
            ),
            (
                "m_flags",
                format!(
                    "arrayMap((v, k) -> toInt32(if(k = ((p.p_mo % length(p.m_state)) + 1), \
                     {}, v)), p.m_flags, arrayEnumerate(p.m_flags))",
                    MF_SOLID | MF_SHOOTABLE
                ),
            ),
            (
                "m_health",
                format!(
                    "arrayMap((v, k) -> toInt32(if(k = ((p.p_mo % length(p.m_state)) + 1), \
                     {TARGET_HEALTH}, v)), p.m_health, arrayEnumerate(p.m_health))"
                ),
            ),
            // Tall and wide enough that the aim's own vertical slope
            // reaches it whatever the level's own spawn gave that slot.
            (
                "m_height",
                "arrayMap((v, k) -> toInt32(if(k = ((p.p_mo % length(p.m_state)) + 1), \
                 3670016, v)), p.m_height, arrayEnumerate(p.m_height))"
                    .to_owned(),
            ),
            (
                "m_radius",
                "arrayMap((v, k) -> toInt32(if(k = ((p.p_mo % length(p.m_state)) + 1), \
                 1310720, v)), p.m_radius, arrayEnumerate(p.m_radius))"
                    .to_owned(),
            ),
        ];
        statements.extend(
            seed::row(&db, arm.at, 0, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        statements.push(sim::tick::run_statement(
            &db,
            &[Input::keys(arm.at + 1, 0, (0, 0))],
        ));
    }
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let wanted: Vec<String> = arms.iter().map(|arm| (arm.at + 1).to_string()).collect();
    let rows: Vec<Punched> = fixture
        .rows(&format!(
            "SELECT tic, psp_state, m_angle, m_health, unresolved, p_mo \
             FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), arms.len(), "every arm ran");
    let p_mo = rows[0].p_mo as usize;
    // The target sits one slot after the player, wrapping the way the
    // seed's own overrides do.
    let target_slot = (p_mo % rows[0].m_health.len()) + 1;

    let at = |name: &str| {
        let arm = arms.iter().find(|a| a.name == name).unwrap();
        rows.iter().find(|row| row.tic == arm.at + 1).unwrap()
    };

    // `(P_Random()%10+1)<<1` off `rndtable`, doubled again for berserk:
    // exact against `prndindex = 0`, the seed's own value.
    let rndtable = clickdoom_native::tables::table("rndtable")
        .expect("the table is committed")
        .ints("value")
        .expect("value is an integer");
    let damage_half = rndtable[1] % 10 + 1;
    let damage = damage_half * 2;
    let berserk_damage = damage * 10;

    let connects = at("connects");
    assert_eq!(connects.psp_state[0], S_PUNCH2, "the punch runs");
    assert_eq!(connects.unresolved, 0);
    assert_eq!(
        connects.m_health[target_slot - 1],
        (TARGET_HEALTH - damage) as i32,
        "the punch's own damage roll, not berserk"
    );
    let expected_angle = point_to_angle(TARGET_X, TARGET_Y) as u32;
    assert_eq!(
        connects.m_angle[p_mo - 1],
        expected_angle,
        "a connecting punch turns the player to face what it hit"
    );

    let berserk = at("connects_berserk");
    assert_eq!(berserk.unresolved, 0);
    assert_eq!(
        berserk.m_health[target_slot - 1],
        (TARGET_HEALTH - berserk_damage) as i32,
        "damage *= 10 under berserk strength, not *2"
    );
    assert_eq!(
        berserk.m_angle[p_mo - 1],
        expected_angle,
        "a connecting berserk punch turns the player too"
    );

    let misses = at("misses");
    assert_eq!(misses.unresolved, 0);
    assert_eq!(
        misses.m_angle[p_mo - 1],
        0,
        "a punch that reaches nothing leaves the player's own angle alone"
    );
}
