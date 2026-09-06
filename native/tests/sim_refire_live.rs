//! `A_ReFire` and `A_FireCGun`, on a real ClickHouse server.
//!
//! Each is seeded straight from the level's own first row rather than run
//! forward from a demo, since every field the two read (the readied
//! weapon, its ammunition, the state cycle) is one this overrides.
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

/// `info.c`: the pistol's own state cycle.
const S_PISTOL1: i32 = 13;
const S_PISTOL2: i32 = 14;
const S_PISTOL3: i32 = 15;
const S_PISTOL4: i32 = 16;

/// `info.c`: the chaingun's own state cycle.
const S_CHAIN: i32 = 49;
const S_CHAIN1: i32 = 52;
const S_CHAIN2: i32 = 53;
const S_CHAINFLASH1: i32 = 55;
const S_CHAINFLASH2: i32 = 56;

/// `doomdef.h`
const WP_PISTOL: i32 = 1;
const WP_CHAINGUN: i32 = 3;
const WP_NOCHANGE: i32 = 10;
/// `d_items.c`: both the pistol and the chaingun eat `am_clip`, one-based
/// for `p_ammo`.
const AM_CLIP: usize = 1;

/// One arm: its name, where the copy of tic 0 lands, the psprite state it
/// seeds the weapon sprite to, the ammunition it leaves in the clip, and
/// whether the fire key is down for the tic that runs from it.
struct Arm {
    name: &'static str,
    at: u32,
    weapon: i32,
    state: i32,
    ammo: i32,
    fires: bool,
}

#[derive(Row, Deserialize)]
struct Ran {
    tic: u32,
    psp_state: Vec<i32>,
    ammo: i32,
    refire: i32,
    extralight: i32,
    unresolved: u64,
}

async fn run_arms(fixture: &Fixture, db: &str, arms: &[Arm]) -> Vec<Ran> {
    let mut statements: Vec<sql::Statement> = Vec::new();
    for arm in arms {
        let overrides: Vec<(&str, String)> = vec![
            ("p_readyweapon", format!("toInt32({})", arm.weapon)),
            ("p_pendingweapon", format!("toInt32({WP_NOCHANGE})")),
            (
                "psp_state",
                format!("CAST([{}, -1], 'Array(Int32)')", arm.state),
            ),
            ("psp_tics", "CAST([1, -1], 'Array(Int32)')".to_owned()),
            (
                "p_ammo",
                format!(
                    "arrayMap((v, k) -> toInt32(if(k = {AM_CLIP}, {}, v)), \
                     p.p_ammo, arrayEnumerate(p.p_ammo))",
                    arm.ammo
                ),
            ),
            ("p_refire", "toInt32(0)".to_owned()),
            ("p_health", "toInt32(100)".to_owned()),
            ("p_extralight", "toInt32(0)".to_owned()),
        ];
        statements.extend(
            seed::row(db, arm.at, 0, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        let keys = if arm.fires { key::FIRE } else { 0 };
        statements.push(sim::tick::run_statement(
            db,
            &[Input::keys(arm.at + 1, keys, (0, 0))],
        ));
    }
    if let Err(error) = fixture.execute(&statements).await {
        panic!("{error}");
    }

    let wanted: Vec<String> = arms.iter().map(|arm| (arm.at + 1).to_string()).collect();
    fixture
        .rows(&format!(
            "SELECT tic, psp_state, p_ammo[{AM_CLIP}] AS ammo, p_refire AS refire, \
             p_extralight AS extralight, unresolved \
             FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await
}

/// `A_ReFire`'s own two branches: it fires again and counts `refire` up
/// while the trigger stays down, and drops back to 0 the tic it does not,
/// whether that is because the trigger came up or because the clip ran
/// dry in between.
#[tokio::test]
async fn a_refire_counts_while_the_trigger_stays_down() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_refire").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    // Seeded at `S_PISTOL3`, one tic from its own `nextstate`, so the tic
    // this runs is the one `A_ReFire` (`S_PISTOL4`) itself decides.
    let arms = [
        Arm {
            name: "refires",
            at: 200,
            weapon: WP_PISTOL,
            state: S_PISTOL3,
            ammo: 50,
            fires: true,
        },
        Arm {
            name: "stops",
            at: 300,
            weapon: WP_PISTOL,
            state: S_PISTOL3,
            ammo: 50,
            fires: false,
        },
        Arm {
            name: "empty",
            at: 400,
            weapon: WP_PISTOL,
            state: S_PISTOL3,
            ammo: 0,
            fires: true,
        },
    ];
    let rows = run_arms(&fixture, &db, &arms).await;
    fixture.finish().await;

    assert_eq!(rows.len(), arms.len(), "every arm ran");
    let at = |name: &str| {
        let arm = arms.iter().find(|a| a.name == name).unwrap();
        rows.iter().find(|row| row.tic == arm.at + 1).unwrap()
    };

    let refires = at("refires");
    assert_eq!(
        (refires.psp_state[0], refires.refire, refires.unresolved),
        (S_PISTOL1, 1, 0),
        "a held trigger fires again and counts the refire up"
    );

    // `A_ReFire` does not call `P_SetPsprite` on its own account, so a tic
    // that does not fire again leaves the sprite exactly where entering
    // `S_PISTOL4` this tic already put it, waiting out its own tics
    // before `S_PISTOL`'s ready state is a cycle away.
    let stops = at("stops");
    assert_eq!(
        (stops.psp_state[0], stops.refire, stops.unresolved),
        (S_PISTOL4, 0, 0),
        "a trigger let go of stays on the frame it entered and the count resets"
    );

    let empty = at("empty");
    assert_eq!(
        empty.unresolved,
        sim::unresolved::PSP_STUCK,
        "an empty clip reaches P_CheckAmmo's own weapon change, which this does not write"
    );
}

/// `A_FirePistol`'s own accuracy: `!player->refire`, read from the count
/// `A_ReFire`'s own entry left rather than from the tic's starting row.
#[tokio::test]
async fn a_held_pistols_second_shot_spreads() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_refire_accuracy").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    // Seeded at `S_PISTOL1` so the tic advances into `S_PISTOL2`, where
    // `A_FirePistol` itself runs; `p_refire` is the one field the two
    // arms differ by.
    let mut statements: Vec<sql::Statement> = Vec::new();
    for (refire, at) in [(0, 200), (1, 300)] {
        let overrides: Vec<(&str, String)> = vec![
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
            // Every other mobj is stopped outright, tics included, so
            // nothing but the shot itself draws a number this tic: a
            // thinker still standing would run its own actions and blur
            // the count this checks. Moved off too, since a trace tests
            // every mobj it is given whatever its own state carries.
            (
                "m_state",
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo, v, 0)), \
                 p.m_state, arrayEnumerate(p.m_state))"
                    .to_owned(),
            ),
            (
                "m_tics",
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo, v, -1)), \
                 p.m_tics, arrayEnumerate(p.m_tics))"
                    .to_owned(),
            ),
            (
                "m_x",
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo, v, -2000000000)), \
                 p.m_x, arrayEnumerate(p.m_x))"
                    .to_owned(),
            ),
            ("p_refire", format!("toInt32({refire})")),
            ("p_health", "toInt32(100)".to_owned()),
        ];
        statements.extend(
            seed::row(&db, at, 0, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        statements.push(sim::tick::run_statement(
            &db,
            &[Input::keys(at + 1, key::FIRE, (0, 0))],
        ));
    }
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    #[derive(Row, Deserialize)]
    struct Fired {
        psp_state: Vec<i32>,
        prndindex: u8,
        unresolved: u64,
        next_seq: u32,
        mobj_count: u64,
    }
    let rows: Vec<Fired> = fixture
        .rows(&format!(
            "SELECT psp_state, prndindex, unresolved, next_seq, \
             length(m_state) AS mobj_count \
             FROM {db}.native_state WHERE tic IN (201, 301) ORDER BY tic"
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), 2, "both arms ran");
    let accurate = &rows[0];
    let spread = &rows[1];

    assert_eq!(accurate.psp_state[0], S_PISTOL2, "the first arm fired");
    assert_eq!(spread.psp_state[0], S_PISTOL2, "the second arm fired too");
    assert_eq!(accurate.unresolved, 0);
    assert_eq!(spread.unresolved, 0);
    // Both arms are seeded from the same row and fire at the same wall or
    // thing, so whatever the shot itself reaches is identical between them;
    // only the two extra numbers a spread angle draws tell the two apart.
    assert_eq!(
        (accurate.next_seq, accurate.mobj_count),
        (spread.next_seq, spread.mobj_count),
        "both arms fire the same shot into the same spot"
    );
    assert_eq!(
        spread.prndindex.wrapping_sub(accurate.prndindex),
        2,
        "a held trigger's second shot spreads and draws the angle's own two numbers, \
         which an unheld trigger's first shot does not"
    );
}

/// `A_FireCGun`'s own two firing frames share the routine, and only the
/// first is behind `P_CheckAmmo`: the clip can run out between them, and
/// the second returns without firing, decrementing ammunition, or putting
/// the flash sprite anywhere.
#[tokio::test]
async fn a_fire_cgun_stops_mid_burst_when_the_clip_runs_out() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_cgun").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    // Seeded at `S_CHAIN1` (the burst's first frame) so the tic advances
    // into `S_CHAIN2`, the burst's second, where `A_FireCGun` runs again.
    let arms = [
        Arm {
            name: "second_shot_fires",
            at: 200,
            weapon: WP_CHAINGUN,
            state: S_CHAIN1,
            ammo: 5,
            fires: true,
        },
        Arm {
            name: "clip_ran_out",
            at: 300,
            weapon: WP_CHAINGUN,
            state: S_CHAIN1,
            ammo: 0,
            fires: true,
        },
        // The ready state fires the burst's own first shot, `S_CHAIN1`
        // itself, in the same tic `A_WeaponReady` redirects into it.
        Arm {
            name: "first_shot_fires",
            at: 400,
            weapon: WP_CHAINGUN,
            state: S_CHAIN,
            ammo: 5,
            fires: true,
        },
    ];
    let rows = run_arms(&fixture, &db, &arms).await;
    fixture.finish().await;

    assert_eq!(rows.len(), arms.len(), "every arm ran");
    let at = |name: &str| {
        let arm = arms.iter().find(|a| a.name == name).unwrap();
        rows.iter().find(|row| row.tic == arm.at + 1).unwrap()
    };

    let first = at("first_shot_fires");
    assert_eq!(
        (first.psp_state[0], first.ammo, first.unresolved),
        (S_CHAIN1, 4, 0),
        "the burst's first shot fires from the ready state and spends a round"
    );
    assert_eq!(
        first.psp_state[1], S_CHAINFLASH1,
        "and flashes the burst's own first frame"
    );
    assert_eq!(first.extralight, 1, "S_CHAINFLASH1 carries A_Light1");

    let second = at("second_shot_fires");
    assert_eq!(
        (second.psp_state[0], second.ammo, second.unresolved),
        (S_CHAIN2, 4, 0),
        "the burst's second shot fires on its own frame and spends a round"
    );
    assert_eq!(
        second.psp_state[1], S_CHAINFLASH2,
        "and flashes the burst's own second frame, not the first's"
    );
    assert_eq!(second.extralight, 2, "S_CHAINFLASH2 carries A_Light2");

    let empty = at("clip_ran_out");
    assert_eq!(
        (empty.psp_state[0], empty.ammo, empty.unresolved),
        (S_CHAIN2, 0, 0),
        "the second shot advances the sprite without firing, spending \
         nothing and leaving the tic resolved"
    );
    assert_eq!(
        empty.psp_state[1], -1,
        "and puts nothing in the flash sprite"
    );
    assert_eq!(
        empty.extralight, 0,
        "a dry burst does not brighten the screen"
    );
}
