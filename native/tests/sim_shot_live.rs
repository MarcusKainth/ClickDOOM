//! `A_WeaponReady`'s branches into `P_FireWeapon`, on a real ClickHouse
//! server.
//!
//! `demo3` fires a shotgun with ammunition in hand from a mobj that is not
//! in its attack frames, so the three branches that decide otherwise are
//! seeded into a state row and one tic is run from each: an empty
//! magazine, a launcher whose button was never let go of, and a mobj
//! already standing in an attack frame.
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

use support::db::Fixture;
use support::seed;

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
/// whether the fire key is down for the tic that runs from it. The tics
/// are far apart so the arms cannot read each other's rows.
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
    unresolved: u8,
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
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let launcher: Vec<(&str, String)> = vec![
        ("p_readyweapon", format!("toInt32({WP_MISSILE})")),
        (
            "psp_state",
            format!("CAST([{MISSILE_READY}, -1], 'Array(Int32)')"),
        ),
        ("psp_tics", "CAST([1, -1], 'Array(Int32)')".to_owned()),
        ("p_ammo", "arrayMap(v -> toInt32(50), p.p_ammo)".to_owned()),
    ];
    let mut statements: Vec<sql::Statement> = Vec::new();
    for (arm, at, fire) in ARMS {
        let mut overrides: Vec<(&str, String)> = match arm {
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
        // The launcher fires again only after its button comes up, so the
        // two launcher arms differ by that one column.
        if arm.starts_with("launcher") {
            let down = i32::from(arm == "launcher_held");
            overrides.push(("p_attackdown", format!("toUInt8({down})")));
        }
        statements.extend(
            seed::row(&db, at, BEFORE, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        let keys = if fire { key::FIRE } else { 0 };
        statements.push(sim::tick::run_statement(
            &db,
            &[Input::keys(at + 1, keys, (0, 0))],
        ));
    }
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let wanted: Vec<String> = ARMS.iter().map(|(_, at, _)| (at + 1).to_string()).collect();
    let rows: Vec<Ran> = fixture
        .rows(&format!(
            "SELECT tic, psp_state, m_state[p_mo] AS pl_state, m_tics[p_mo] AS pl_tics, \
             p_attackdown AS attackdown, \
             toUInt64(countEqual(sec_soundtarget, p_mo)) AS heard, unresolved \
             FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), ARMS.len(), "every arm ran");
    let at = |arm: &str| {
        let (_, tic, _) = ARMS.iter().find(|(name, _, _)| *name == arm).unwrap();
        rows.iter().find(|row| row.tic == tic + 1).unwrap()
    };

    let fires = at("fires");
    assert_eq!(
        (fires.psp_state[0], fires.pl_state, fires.attackdown),
        (SGUN_ATTACK, S_PLAY_ATK1, 1),
        "the shotgun fires and the mobj enters its attack frames"
    );
    assert!(fires.heard > 0, "the shot is heard somewhere");
    assert_eq!(fires.unresolved, 0, "and the tic is carried through");

    let noammo = at("noammo");
    assert_ne!(
        noammo.pl_state, S_PLAY_ATK1,
        "`P_FireWeapon` counts the ammunition before it touches the mobj, \
         so an empty magazine leaves the frames alone"
    );
    assert_ne!(
        noammo.psp_state[0], SGUN_ATTACK,
        "an empty magazine does not reach the firing frames"
    );
    assert_eq!(noammo.heard, 0, "and nothing hears it");
    assert_eq!(
        noammo.unresolved, 1,
        "and the weapon it would pick instead says the tic could not be produced"
    );

    let frames = at("attack_frames");
    assert_eq!(
        (frames.pl_state, frames.pl_tics),
        (S_PLAY, -1),
        "a mobj standing in an attack frame is put back where it waits"
    );
    assert_eq!(frames.heard, 0, "with the fire key up nothing is heard");
    assert_eq!(frames.unresolved, 0);

    let held = at("launcher_held");
    let free = at("launcher_free");
    assert_eq!(
        free.psp_state[0], MISSILE_ATTACK,
        "a launcher whose button came up fires"
    );
    assert_eq!(
        held.psp_state[0], MISSILE_READY,
        "and one whose button never did stays ready"
    );
    assert_eq!(
        (held.pl_state, held.heard),
        (noammo.pl_state, 0),
        "so its mobj cycles the way one that did not fire does, and \
         nothing hears it"
    );
}
