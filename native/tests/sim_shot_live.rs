//! `A_WeaponReady`'s branches into `P_FireWeapon`, on a real ClickHouse
//! server.
//!
//! `demo3` fires a shotgun with ammunition in hand from a mobj that is not
//! in its attack frames, so the three branches that decide otherwise are
//! seeded into a state row and one tic is run from each: an empty
//! magazine, a launcher whose button was never let go of, and a mobj
//! already standing in an attack frame. Every arm runs in one session, so
//! the suite pays the tic statement's analysis once.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::sql::sim::tick::Input;
use clickdoom_native::{load, sql, wad::Wad};
use clickdoom_spec::native_state::key;
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::arms::{Arm, Arms};
use support::db::Fixture;
use support::resident::Session;

/// The last tic the demo drives. The row gametic 139 leaves holds the
/// shotgun ready, ammunition for it, and the player's mobj in a running
/// frame, which is the world `A_WeaponReady` fires from at 140.
const BEFORE: u32 = 139;

/// `doomdef.h`: `wp_missile`, whose button has to come up between shots.
const WP_MISSILE: i32 = 4;
/// `d_items.c`: the states the launcher's own entry names, which the arm
/// that holds it has to start from.
const MISSILE_READY: i32 = 57;
const MISSILE_ATTACK: i32 = 60;
/// `info.h`: the shotgun's first firing frame, and the player's first
/// attack frame.
const SGUN_ATTACK: i32 = 21;
const S_PLAY: i32 = 149;
const S_PLAY_ATK1: i32 = 154;

/// One arm per seeded row: its name, where the copy of `BEFORE` lands, and
/// whether the fire key is down for the tic that runs from it.
const ARMS: [(&str, u32, bool); 5] = [
    ("fires", 200, true),
    ("noammo", 300, true),
    ("attack_frames", 400, false),
    ("launcher_held", 500, true),
    ("launcher_free", 600, true),
];

#[derive(Row, Deserialize)]
struct Ran {
    tic: u32,
    psp_state: Vec<i32>,
    pl_state: i32,
    pl_tics: i32,
    attackdown: u8,
    heard: u64,
    unresolved: u64,
}

fn arms() -> Arms {
    let launcher: Vec<(&str, String)> = vec![
        ("p_readyweapon", format!("toInt32({WP_MISSILE})")),
        (
            "psp_state",
            format!("CAST([{MISSILE_READY}, -1], 'Array(Int32)')"),
        ),
        ("psp_tics", "CAST([1, -1], 'Array(Int32)')".to_owned()),
        ("p_ammo", "arrayMap(v -> toInt32(50), p.p_ammo)".to_owned()),
    ];
    let arms = ARMS
        .into_iter()
        .map(|(name, at, fire)| {
            let mut overrides: Vec<(&str, String)> = match name {
                "noammo" => vec![("p_ammo", "arrayMap(v -> toInt32(0), p.p_ammo)".to_owned())],
                "attack_frames" => vec![(
                    "m_state",
                    format!(
                        "arrayMap((v, k) -> toInt32(if(k = p.p_mo, {S_PLAY_ATK1}, v)), \
                         p.m_state, arrayEnumerate(p.m_state))"
                    ),
                )],
                "launcher_held" | "launcher_free" => launcher.clone(),
                _ => Vec::new(),
            };
            // The launcher fires again only after its button comes up, so
            // the two launcher arms differ by that one column.
            if name.starts_with("launcher") {
                let down = i32::from(name == "launcher_held");
                overrides.push(("p_attackdown", format!("toUInt8({down})")));
            }
            let keys = if fire { key::FIRE } else { 0 };
            Arm {
                name,
                from: BEFORE,
                overrides,
                at,
                inputs: vec![Input::keys(at + 1, keys, (0, 0))],
            }
        })
        .collect();
    Arms::new((1..=BEFORE).map(Input::demo).collect(), arms)
}

#[tokio::test]
async fn the_weapon_fires_only_where_the_engine_fires_it() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_shot").await;
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

    let columns = "tic, psp_state, m_state[p_mo] AS pl_state, m_tics[p_mo] AS pl_tics, \
                   p_attackdown AS attackdown, \
                   toUInt64(countEqual(sec_soundtarget, p_mo)) AS heard, unresolved";
    let mut ran: Vec<Vec<Ran>> = Vec::new();
    for arm in arms.all() {
        ran.push(arm.rows(&fixture, "native_state", columns).await);
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
    let at = |name: &str| &ran[arms.all().iter().position(|arm| arm.name == name).unwrap()][1];
    let mut checks = arms.checks();

    let fires = at("fires");
    let mut arm = checks.arm("fires");
    arm.eq(
        (fires.psp_state[0], fires.pl_state, fires.attackdown),
        (SGUN_ATTACK, S_PLAY_ATK1, 1),
        "the shotgun fires and the mobj enters its attack frames",
    );
    arm.check(fires.heard > 0, "the shot is heard somewhere");
    arm.eq(fires.unresolved, 0, "and the tic is carried through");

    let noammo = at("noammo");
    let mut arm = checks.arm("noammo");
    arm.check(
        noammo.pl_state != S_PLAY_ATK1,
        "`P_FireWeapon` counts the ammunition before it touches the mobj, \
         so an empty magazine leaves the frames alone",
    );
    arm.check(
        noammo.psp_state[0] != SGUN_ATTACK,
        "an empty magazine does not reach the firing frames",
    );
    arm.eq(noammo.heard, 0, "and nothing hears it");
    arm.eq(
        noammo.unresolved,
        sim::unresolved::PSP_STUCK,
        "and the weapon it would pick instead says the tic could not be produced",
    );

    let frames = at("attack_frames");
    let mut arm = checks.arm("attack_frames");
    arm.eq(
        (frames.pl_state, frames.pl_tics),
        (S_PLAY, -1),
        "a mobj standing in an attack frame is put back where it waits",
    );
    arm.eq(frames.heard, 0, "with the fire key up nothing is heard");
    arm.eq(frames.unresolved, 0, "and the tic is carried through");

    let held = at("launcher_held");
    let free = at("launcher_free");
    checks.arm("launcher_free").eq(
        free.psp_state[0],
        MISSILE_ATTACK,
        "a launcher whose button came up fires",
    );
    let mut arm = checks.arm("launcher_held");
    arm.eq(
        held.psp_state[0],
        MISSILE_READY,
        "and one whose button never did stays ready",
    );
    arm.eq(
        (held.pl_state, held.heard),
        (noammo.pl_state, 0),
        "so its mobj cycles the way one that did not fire does, and \
         nothing hears it",
    );
    checks.finish();
}
