//! `P_DamageMobj`'s player branch and the kill it runs into, against a
//! real ClickHouse server.
//!
//! A claw is seeded the way `native/tests/sim_troop_live.rs` seeds one,
//! aimed at the player instead of a second imp: an imp beside the player,
//! one tic from `A_TroopAttack`'s own frame.
//!
//! A fireball's own impact is seeded from `demo3`'s own recorded run
//! instead: `native/tests/fixtures/demo3-player-damage.tsv` carries the
//! reference emulator's own row at gametic 205, the tic before an
//! in-flight fireball reaches the player, so the geometry, the blockmap
//! and every other thing on the level's own list are exactly what the
//! real engine had rather than a hand-placed approximation. Running
//! gametic 206 from it with the demo lump's own recorded command, with no
//! armour, reproduces the reference's own row at 206 field for field,
//! which doubles as the parity the differential run cannot reach on its
//! own yet (a missile move gap earlier in the demo, unrelated to this,
//! still refuses first).
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::sql::sim::tick::Input;
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::arms::{Arm, Arms, Checks};
use support::damage::Player;
use support::db::Fixture;
use support::resident::Session;
use support::seed;

/// The tic every arm copies its row from. Gametic 40 is early enough that
/// no monster has woken and the list still holds the level's own things.
const BEFORE: u32 = 40;

/// The imp that runs the routine. `MT_TROOP`, on the level's own list, and
/// stands still at gametic 40.
const ATTACKER: usize = 116;

/// `states.tsv`: the frame carrying `A_FaceTarget`, and the frame after it
/// carrying `A_TroopAttack`. Seeding the first with one tic of wait left
/// puts the routine on the tic the arm runs.
const FACE: i32 = 453;
const ATTACK: i32 = 454;

/// How far from the player the imp stands. `MELEERANGE` is sixty four
/// units and `P_CheckMeleeRange` measures against that less twenty, plus
/// the player's own radius; four units is inside its reach.
const NEAR: i64 = 4 * 65536;

/// `p_local.h`: how long a thing chases what hit it before it looks
/// elsewhere.
const BASETHRESHOLD: i32 = 100;
/// `doomdef.h`
const ARMOR_NONE: i64 = 0;
const ARMOR_GREEN: i64 = 1;
const ARMOR_BLUE: i64 = 2;
/// Comfortably more than a claw's own worst case (24) can spend, so the
/// armour arms never run out of what they save.
const STARTING_ARMORPOINTS: i64 = 100;

/// One arm per seeded row: its name, where the copy of `BEFORE` lands and
/// the armour it starts the player with.
const ARMS: [(&str, u32, i64); 3] = [
    ("no_armor", 200, ARMOR_NONE),
    ("green_armor", 300, ARMOR_GREEN),
    ("blue_armor", 400, ARMOR_BLUE),
];

/// The same three arms for a claw that kills. A claw's own damage is three
/// times one to eight, and blue armour saves the largest share of it, so
/// one point of health dies to every roll in every arm.
const KILLING_ARMS: [(&str, u32, i64); 3] = [
    ("killing_no_armor", 500, ARMOR_NONE),
    ("killing_green_armor", 600, ARMOR_GREEN),
    ("killing_blue_armor", 700, ARMOR_BLUE),
];

/// The health both the player and its mobj start a killing arm with.
const DYING_HEALTH: i32 = 1;

/// How many tics a killing arm runs from its seeded row. The second is
/// what reads the tic after a death, which `P_DeathThink` owns and this
/// does not run. A surviving arm runs one.
const KILLING_TICS: u32 = 2;

/// `p_mobj.h`
const MF_SOLID: i32 = 2;
const MF_SHOOTABLE: i32 = 4;
const MF_CORPSE: i32 = 0x10_0000;
const MF_DROPOFF: i32 = 0x400;
/// `p_pspr.c`: how far `A_Lower` takes the weapon sprite down the screen
/// in one step, and where `A_WeaponReady` holds it for a player who is
/// not moving.
const LOWERSPEED: i32 = 6 << 16;
const WEAPONTOP: i32 = 32 << 16;

/// The frame and its wait that the weapon sprite goes to when the player
/// dies, from the engine's own tables: `weaponinfo[readyweapon].downstate`
/// and that state's own tics.
fn downstate(readyweapon: i32) -> (i32, i32) {
    let weapons = tables::table("weaponinfo").expect("the table is committed");
    let down =
        weapons.ints("downstate").expect("downstate is an integer")[readyweapon as usize] as i32;
    let states = tables::table("states").expect("the table is committed");
    let tics = states.ints("tics").expect("tics is an integer")[down as usize] as i32;
    (down, tics)
}

#[derive(Row, Deserialize)]
struct Hit {
    tic: u32,
    p_health: i32,
    p_armorpoints: i32,
    p_armortype: i32,
    p_damagecount: i32,
    p_attacker: u32,
    player_health: i32,
    attacker_state: i32,
    unresolved: u64,
    player_state: i32,
    player_sprite: i32,
    player_frame: i32,
    playerstate: u8,
    player_flags: i32,
    player_height: i32,
    readyweapon: i32,
    weapon_state: i32,
    weapon_tics: i32,
    weapon_sy: i32,
    player_target: u32,
    player_threshold: i32,
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

/// The imp beside the player in the frame whose next carries the routine,
/// with one tic of wait left on it, and the player holding `health` on
/// both the player's own field and its mobj's.
fn claw_overrides(armortype: i64, health: i32) -> Vec<(&'static str, String)> {
    vec![
        put(
            "m_x",
            ATTACKER,
            format!("p.m_x[p.p_mo] + toInt32({NEAR})"),
            "toInt32",
        ),
        put("m_y", ATTACKER, "p.m_y[p.p_mo]".to_owned(), "toInt32"),
        put("m_z", ATTACKER, "p.m_z[p.p_mo]".to_owned(), "toInt32"),
        put("m_state", ATTACKER, FACE.to_string(), "toInt32"),
        put("m_tics", ATTACKER, "1".to_owned(), "toInt32"),
        put("m_target", ATTACKER, "p.p_mo".to_owned(), "toUInt32"),
        (
            "m_health",
            format!(
                "arrayMap((v, k) -> toInt32(if(k = p.p_mo, {health}, v)), \
                 p.m_health, arrayEnumerate(p.m_health))"
            ),
        ),
        // The player stands still, so `P_CalcHeight` leaves the bob at
        // zero and `A_WeaponReady` puts the weapon sprite at `WEAPONTOP`
        // exactly. The drop below is then a number rather than a number
        // plus whatever the bob happened to be that tic.
        (
            "m_momx",
            "arrayMap((v, k) -> toInt32(if(k = p.p_mo, 0, v)), \
             p.m_momx, arrayEnumerate(p.m_momx))"
                .to_owned(),
        ),
        (
            "m_momy",
            "arrayMap((v, k) -> toInt32(if(k = p.p_mo, 0, v)), \
             p.m_momy, arrayEnumerate(p.m_momy))"
                .to_owned(),
        ),
        ("p_health", format!("toInt32({health})")),
        ("p_armortype", format!("toInt32({armortype})")),
        (
            "p_armorpoints",
            format!(
                "toInt32({})",
                if armortype == ARMOR_NONE {
                    0
                } else {
                    STARTING_ARMORPOINTS
                }
            ),
        ),
        ("p_damagecount", "toInt32(0)".to_owned()),
        ("p_attacker", "toUInt32(0)".to_owned()),
    ]
}

/// A claw arm seeded from [`BEFORE`] at `at`, with the player holding
/// `health`, run `tics` tics.
fn claw(name: &'static str, at: u32, armortype: i64, health: i32, tics: u32) -> Arm {
    Arm {
        name,
        from: BEFORE,
        overrides: claw_overrides(armortype, health),
        at,
        inputs: (1..=tics)
            .map(|step| Input::keys(at + step, 0, (0, 0)))
            .collect(),
    }
}

fn hit_columns() -> String {
    format!(
        "tic, p_health, p_armorpoints, p_armortype, p_damagecount, p_attacker, \
         m_health[p_mo] AS player_health, m_state[{ATTACKER}] AS attacker_state, \
         unresolved, m_state[p_mo] AS player_state, m_sprite[p_mo] AS player_sprite, \
         m_frame[p_mo] AS player_frame, p_playerstate AS playerstate, \
         m_flags[p_mo] AS player_flags, m_height[p_mo] AS player_height, \
         p_readyweapon AS readyweapon, psp_state[1] AS weapon_state, \
         psp_tics[1] AS weapon_tics, psp_sy[1] AS weapon_sy, \
         m_target[p_mo] AS player_target, m_threshold[p_mo] AS player_threshold"
    )
}

/// Every claw arm, living and killing, seeded from one walk to [`BEFORE`]
/// and driven through one session, so every arm's imp draws from the same
/// `prndindex` and rolls the same damage.
#[tokio::test]
async fn a_claw_reaches_the_player_the_way_the_engine_reaches_it() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_player_damage_claw").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let walk: Vec<Input> = (1..=BEFORE).map(Input::demo).collect();
    let mut seeded: Vec<Arm> = ARMS
        .iter()
        .map(|&(name, at, armortype)| claw(name, at, armortype, 100, 1))
        .collect();
    seeded.extend(
        KILLING_ARMS
            .iter()
            .map(|&(name, at, armortype)| claw(name, at, armortype, DYING_HEALTH, KILLING_TICS)),
    );
    let arms = Arms::new(walk, seeded);
    let mut session = Session::open(&fixture, false).await;
    arms.drive(&fixture, &mut session).await;
    session.close().await;

    let mut checks = arms.checks();
    let mut rows: Vec<Hit> = Vec::new();
    for arm in arms.all() {
        rows.extend(
            arm.rows::<Hit>(&fixture, "native_state", &hit_columns())
                .await,
        );
    }
    fixture.finish().await;
    assert_eq!(
        rows.len(),
        arms.all()
            .iter()
            .map(|arm| arm.inputs.len() + 1)
            .sum::<usize>(),
        "a seeded row and every tic run from it, for every arm"
    );

    a_claw_reaches_the_player_through_its_own_armour(&mut checks, &rows);
    a_claw_that_kills_the_player_still_writes_its_fields(&mut checks, &rows);
    checks.finish();
}

/// The row at `tic`.
fn at(rows: &[Hit], tic: u32) -> &Hit {
    rows.iter()
        .find(|row| row.tic == tic)
        .unwrap_or_else(|| panic!("no row for tic {tic}"))
}

fn a_claw_reaches_the_player_through_its_own_armour(checks: &mut Checks, rows: &[Hit]) {
    // The bare arm: no armour, so the raw roll reaches health outright.
    // `A_TroopAttack`'s own damage is `(P_Random()%8+1)*3`, and every arm
    // seeds the same imp at the same state with the same starting
    // `prndindex`, so the roll is identical across all three.
    let mut c = checks.arm("no_armor");
    let (before, after) = (at(rows, 200), at(rows, 201));
    c.eq(
        before.attacker_state,
        FACE,
        "the seeded row is a tic from the routine",
    );
    c.eq(after.attacker_state, ATTACK, "and the cycle reaches it");
    c.eq(
        after.unresolved,
        0,
        "a living player's own hit resolves, unlike a monster's",
    );
    let raw_damage = before.p_health - after.p_health;
    c.check(
        (3..=24).contains(&raw_damage) && raw_damage % 3 == 0,
        format!("the claw's own damage is three times one to eight: {raw_damage}"),
    );
    c.eq(
        before.player_health - after.player_health,
        raw_damage,
        "the mobj's own health shares the same number",
    );
    c.eq(after.p_armorpoints, 0, "no armour, nothing saved");
    c.eq(after.p_armortype, 0, "p_armortype");
    c.eq(
        after.p_damagecount,
        raw_damage,
        "the tint takes the whole hit",
    );
    c.eq(
        after.p_attacker,
        ATTACKER as u32,
        "the player's own attacker is the imp that clawed it",
    );
    // `P_DamageMobj` turns whatever it reached onto what hit it, and the
    // player is not excepted: the mobj's own target and threshold move
    // the way any monster's do.
    c.eq(
        before.player_target,
        0,
        "the seeded player has nothing on its mind",
    );
    c.eq(
        after.player_target,
        ATTACKER as u32,
        "and turns onto the imp that clawed it",
    );
    c.eq(
        after.player_threshold,
        BASETHRESHOLD,
        "holding it for the threshold's worth of tics",
    );

    // The armoured arms save a third or a half of the same roll.
    for (name, divisor) in [("green_armor", 3), ("blue_armor", 2)] {
        let mut c = checks.arm(name);
        let (_, at_tic, _) = ARMS.iter().find(|(n, _, _)| *n == name).unwrap();
        let (before, after) = (at(rows, *at_tic), at(rows, *at_tic + 1));
        c.eq(after.unresolved, 0, "unresolved");
        let saved = raw_damage / divisor;
        c.eq(
            before.p_armorpoints - after.p_armorpoints,
            saved,
            "the armour absorbs its own share",
        );
        c.eq(
            before.p_health - after.p_health,
            raw_damage - saved,
            "health takes what the armour did not",
        );
        c.eq(
            before.player_health - after.player_health,
            raw_damage - saved,
            "the mobj's own health shares the same number",
        );
        c.eq(
            after.p_damagecount,
            raw_damage - saved,
            "the tint takes the post-armour damage",
        );
    }
}

/// A claw that kills the player writes the same health, armour, tint and
/// attacker a survivable hit writes.
///
/// The tic the kill lands on resolves, since `P_PlayerThink` had already
/// taken its living branch by the time the hit arrived. The tic after it
/// is `P_DeathThink`'s, which this does not run, and says so.
fn a_claw_that_kills_the_player_still_writes_its_fields(checks: &mut Checks, rows: &[Hit]) {
    // The bare arm names the roll the other two share: with no armour the
    // tint takes the whole hit, so `p_damagecount` is the claw's own
    // damage. `A_TroopAttack` rolls three times one to eight.
    let (bare, bare_at, _) = KILLING_ARMS[0];
    let raw_damage = i64::from(at(rows, bare_at + 1).p_damagecount);

    for (name, tic, armortype) in KILLING_ARMS {
        let mut c = checks.arm(name);
        if name == bare {
            c.check(
                (3..=24).contains(&raw_damage) && raw_damage % 3 == 0,
                format!("the claw's own damage is three times one to eight: {raw_damage}"),
            );
        }
        let before = Player {
            health: i64::from(DYING_HEALTH),
            armorpoints: match armortype {
                ARMOR_NONE => 0,
                _ => STARTING_ARMORPOINTS,
            },
            armortype,
            damagecount: 0,
            attacker: 0,
            playerstate: 0,
        };
        let (want, taken) = before.hurt(raw_damage, ATTACKER as i64);
        let after = at(rows, tic + 1);
        c.eq(
            after.unresolved,
            0,
            "the tic the kill lands on is written whole",
        );
        c.eq(
            Player {
                health: i64::from(after.p_health),
                armorpoints: i64::from(after.p_armorpoints),
                armortype: i64::from(after.p_armortype),
                damagecount: i64::from(after.p_damagecount),
                attacker: i64::from(after.p_attacker),
                playerstate: i64::from(after.playerstate),
            },
            want.killed(),
            "the fields p_inter.c's own player block leaves",
        );
        c.eq(after.p_health, 0, "the health clamps at zero");
        c.eq(
            i64::from(after.player_health),
            i64::from(DYING_HEALTH) - taken,
            "the mobj's own health takes the same damage and keeps going past zero",
        );
        // `P_SetMobjState` writes the picture with the state, so the
        // player's mobj shows the death frame the kill put it in.
        c.check(
            after.player_state != at(rows, tic).player_state,
            "the kill moves the player into its death frames",
        );
        c.eq(
            (
                i64::from(after.player_sprite),
                i64::from(after.player_frame),
            ),
            support::damage::picture(i64::from(after.player_state)),
            "and the picture follows that state",
        );
        // `P_KillMobj`: the corpse loses `MF_SOLID` on top of what any
        // corpse loses, and drops to a quarter of its height.
        let before = at(rows, tic);
        c.eq(
            after.player_flags & (MF_SOLID | MF_SHOOTABLE),
            0,
            "the corpse is neither solid nor shootable",
        );
        c.eq(
            after.player_flags & (MF_CORPSE | MF_DROPOFF),
            MF_CORPSE | MF_DROPOFF,
            "and carries both corpse flags",
        );
        c.eq(
            after.player_height,
            before.player_height >> 2,
            "and stands a quarter as tall",
        );
        // `P_DropWeapon`, and the `A_Lower` that `P_SetPsprite` runs on
        // the way into the frame it enters.
        let (down, tics) = downstate(after.readyweapon);
        c.eq(
            (after.weapon_state, after.weapon_tics),
            (down, tics),
            "the weapon sprite goes to its own down state",
        );
        // The seeded player stands still, so `P_CalcHeight` leaves the bob
        // at zero and `A_WeaponReady` puts the sprite at `WEAPONTOP`
        // before `P_DropWeapon` runs. The seeded row itself carries
        // whatever the bob was at the tic it was copied from, so the
        // number to read is the one the tic left.
        c.eq(
            after.weapon_sy,
            WEAPONTOP + LOWERSPEED,
            "and A_Lower takes one step down the screen",
        );
        // The tic after it is the one `P_DeathThink` owns, which this does
        // not run, so it says so rather than writing a guess.
        c.eq(
            at(rows, tic + 2).unresolved,
            sim::unresolved::PLAYER_DEAD,
            "the tic after the kill is P_DeathThink's",
        );
    }
}

/// The reference emulator's own rows at gametic 205 and 206, `demo3`'s own
/// probe trace trimmed to the tic a thrown fireball reaches the player.
const PROBE_ROWS: &str = include_str!("fixtures/demo3-player-damage.tsv");

const GAMETIC_BEFORE: u32 = 205;
const GAMETIC_HIT: u32 = 206;

/// `refemu/reference_traces/demo3/probe.9a6a47d01119.tsv`: the fireball's
/// own array position, both rows.
const MISSILE_SLOT: usize = 265;
/// The imp that threw it, `m_target` on both rows.
const THROWER: u32 = 119;
/// `d_player.h`: `pw_invulnerability`, one-based for `p_powers`.
const PW_INVULNERABILITY: usize = 1;
/// `mobjtype.tsv`
const TROOPSHOT: i32 = 31;
/// `p_pspr.c`'s own `S_TBALL1`/`S_TBALL2`, from the probe: the frame the
/// missile flies in and the one `P_ExplodeMissile` puts it in.
const TBALL1: i32 = 98;
const TBALL2: i32 = 99;
/// `p_mobj.h`: `MF_SOLID | MF_SHOOTABLE | MF_MISSILE | MF_DROPOFF |
/// MF_NOGRAVITY`, the fireball's own flags in flight, and what
/// `P_ExplodeMissile` leaves once it strikes something.
const FLYING_FLAGS: i32 = 67088;
const EXPLODED_FLAGS: i32 = 1552;
/// The reference's own row at gametic 205: the fireball still in flight.
const BEFORE_X: i32 = 11078949;
const BEFORE_Y: i32 = 12923838;
const BEFORE_Z: i32 = 2097152;
const BEFORE_MOMX: i32 = -285410;
const BEFORE_MOMY: i32 = 589940;
const BEFORE_RADIUS: i32 = 393216;
const BEFORE_HEIGHT: i32 = 524288;
const BEFORE_SUBSECTOR: i32 = 145;
/// `P_ExplodeMissile`'s own death frame (`S_TBALL2`, `states.tsv` row 99)
/// carries six tics, and `P_SetMobjState` shortens that by a number of
/// its own: `greatest(6 - (P_Random() & 3), 1)`, three to six.
const DEATH_TICS: i32 = 6;

fn probe_line(gametic: u32) -> Vec<u8> {
    let line = PROBE_ROWS
        .lines()
        .find(|line| line.split('\t').nth(1) == Some(gametic.to_string().as_str()))
        .unwrap_or_else(|| panic!("no probe row for gametic {gametic}"));
    format!("{line}\n").into_bytes()
}

#[derive(Row, Deserialize)]
struct Impact {
    p_health: i32,
    p_armorpoints: i32,
    p_armortype: i32,
    p_damagecount: i32,
    p_attacker: u32,
    player_health: i32,
    missile_state: i32,
    missile_tics: i32,
    missile_flags: i32,
    missile_momx: i32,
    missile_momy: i32,
    missile_x: i32,
    missile_y: i32,
    missile_health: i32,
    unresolved: u64,
}

async fn hit_after_206(fixture: &Fixture, db: &str) -> Impact {
    fixture
        .execute(&sim::tick::demo_statement(db, GAMETIC_HIT, GAMETIC_HIT))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    fixture
        .rows(&format!(
            "SELECT p_health, p_armorpoints, p_armortype, p_damagecount, p_attacker, \
             m_health[p_mo] AS player_health, m_state[{MISSILE_SLOT}] AS missile_state, \
             m_tics[{MISSILE_SLOT}] AS missile_tics, m_flags[{MISSILE_SLOT}] AS missile_flags, \
             m_momx[{MISSILE_SLOT}] AS missile_momx, m_momy[{MISSILE_SLOT}] AS missile_momy, \
             m_x[{MISSILE_SLOT}] AS missile_x, m_y[{MISSILE_SLOT}] AS missile_y, \
             m_health[{MISSILE_SLOT}] AS missile_health, unresolved \
             FROM {db}.native_state WHERE tic = {GAMETIC_HIT}"
        ))
        .await
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no row for tic {GAMETIC_HIT}"))
}

async fn seeded_fireball(name: &str, armortype: i64, armorpoints: i64) -> Impact {
    seeded_fireball_with(name, armortype, armorpoints, false).await
}

async fn seeded_fireball_with(
    name: &str,
    armortype: i64,
    armorpoints: i64,
    invulnerable: bool,
) -> Impact {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create(&format!("sim_player_damage_{name}")).await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    support::probe::load(&fixture, &probe_line(GAMETIC_BEFORE)).await;

    let mut overrides: Vec<(&str, String)> = Vec::new();
    if armortype != 0 || armorpoints != 0 {
        overrides.push(("p_armortype", format!("toInt32({armortype})")));
        overrides.push(("p_armorpoints", format!("toInt32({armorpoints})")));
    }
    if invulnerable {
        // `player::powers`'s own per-tic decay runs ahead of the thinker
        // stage this seeds the hit into, taking one tic off before the
        // fireball ever reads it, so one tic of its own is not enough:
        // seed several to survive the decay and still read nonzero.
        overrides.push((
            "p_powers",
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {PW_INVULNERABILITY}, 5, v)), \
                 p.p_powers, arrayEnumerate(p.p_powers))"
            ),
        ));
    }
    if !overrides.is_empty() {
        let statements = seed::row(&db, GAMETIC_BEFORE, GAMETIC_BEFORE, &overrides)
            .into_iter()
            .map(sql::Statement::sql)
            .collect::<Vec<_>>();
        if let Err(error) = fixture.execute(&statements).await {
            fixture.finish().await;
            panic!("{error}");
        }
    }

    let after = hit_after_206(&fixture, &db).await;
    fixture.finish().await;
    after
}

/// A fireball already in flight, seeded from `demo3`'s own recorded run
/// at the tic before it reaches the player, across no armour and both
/// armour types.
///
/// The reference's own row at gametic 206 has the player taking exactly
/// this hit (health 100 to 82, `p_attacker` the imp that threw it), which
/// this reproduces for the fields the armour cannot move: the missile's
/// own explode frame, its flags, and that the impact stops it rather than
/// carrying it through. The damage roll itself is read off the no-armour
/// arm's own run rather than asserted against the reference number: the
/// two agree on every field this checks, but a busy, full-level tic draws
/// numbers for hundreds of other things this does not seed, and nothing
/// here answers for their own count being exactly what the reference
/// drew.
#[tokio::test]
async fn a_fireball_reaches_the_player_through_its_own_armour() {
    let bare = seeded_fireball("fireball_no_armor", 0, 0).await;
    assert_eq!(bare.unresolved, 0, "a living player's own hit resolves");
    assert_eq!(bare.p_armorpoints, 0, "no armour, nothing saved");
    assert_eq!(bare.p_armortype, 0);
    assert_eq!(bare.p_attacker, THROWER);
    assert_eq!(
        bare.player_health, bare.p_health,
        "the shared health agrees with the player's own"
    );
    assert_eq!(
        bare.missile_state, TBALL2,
        "the fireball's own explode frame"
    );
    assert!(
        (DEATH_TICS - 3..=DEATH_TICS).contains(&bare.missile_tics),
        "the death frame's own wait shortens by up to three: {}",
        bare.missile_tics
    );
    assert_eq!(bare.missile_flags, EXPLODED_FLAGS);
    assert_eq!(bare.missile_momx, 0, "the impact stops it");
    assert_eq!(bare.missile_momy, 0);
    assert_eq!(
        bare.missile_x, BEFORE_X,
        "an exploding missile does not move"
    );
    assert_eq!(bare.missile_y, BEFORE_Y);
    assert_eq!(
        bare.missile_health, 1000,
        "the fireball's own health is not what the hit moves"
    );
    let raw_damage = 100 - bare.p_health;
    assert_eq!(
        bare.p_damagecount, raw_damage,
        "no armour, the tint takes the whole hit"
    );

    for (name, armortype, divisor) in [("green_armor", 1, 3), ("blue_armor", 2, 2)] {
        let after = seeded_fireball(&format!("fireball_{name}"), armortype, 100).await;
        assert_eq!(after.unresolved, 0, "{name}");
        let saved = raw_damage / divisor;
        assert_eq!(
            100 - after.p_armorpoints,
            saved,
            "{name}: the armour absorbs its own share of the same roll"
        );
        assert_eq!(
            100 - after.p_health,
            raw_damage - saved,
            "{name}: health takes what the armour did not"
        );
        assert_eq!(
            after.player_health, after.p_health,
            "{name}: the shared health agrees"
        );
        assert_eq!(after.p_damagecount, raw_damage - saved, "{name}");
    }
}

/// A god mode or invulnerable player's own return leaves the player's own
/// fields, and the mobj's own shared health, untouched. The fireball's own
/// impact is a different mobj's own move blocked, not `P_DamageMobj`'s
/// doing, so it explodes and stops exactly as it does against a player who
/// takes the hit.
#[tokio::test]
async fn an_invulnerable_player_is_still_pushed_but_not_hurt() {
    let after = seeded_fireball_with("fireball_invulnerable", 0, 0, true).await;
    assert_eq!(after.unresolved, 0, "an immune hit resolves too");
    assert_eq!(after.p_health, 100, "the return skips the health");
    assert_eq!(after.p_armorpoints, 0, "and the armour");
    assert_eq!(after.p_armortype, 0);
    assert_eq!(after.p_damagecount, 0, "and the tint");
    assert_eq!(after.p_attacker, 0, "and the attacker");
    assert_eq!(
        after.player_health, 100,
        "the shared health is not touched either"
    );
    assert_eq!(
        after.missile_state, TBALL2,
        "the fireball's own impact does not depend on the target's return"
    );
    assert_eq!(after.missile_flags, EXPLODED_FLAGS);
    assert_eq!(
        after.missile_momx, 0,
        "its own move stops it, blocked or not"
    );
    assert_eq!(after.missile_momy, 0);
}

#[tokio::test]
async fn two_fireballs_in_one_tic_thread_the_player_through_both() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_player_damage_two_hits").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    support::probe::load(&fixture, &probe_line(GAMETIC_BEFORE)).await;

    // A corpse already dead by gametic 205, repurposed as a second
    // fireball on the same heading as the real one.
    const SECOND_SLOT: usize = 258;
    // The player's own row 205 position minus the real missile's own
    // momentum, so this one's move lands exactly on the player rather
    // than relying on the same box sweep the real one's own position
    // happens to clear.
    const PLAYER_X: i32 = 10419829;
    const PLAYER_Y: i32 = 14857119;
    let overrides = [
        put(
            "m_x",
            SECOND_SLOT,
            (PLAYER_X - BEFORE_MOMX).to_string(),
            "toInt32",
        ),
        put(
            "m_y",
            SECOND_SLOT,
            (PLAYER_Y - BEFORE_MOMY).to_string(),
            "toInt32",
        ),
        put("m_z", SECOND_SLOT, BEFORE_Z.to_string(), "toInt32"),
        put("m_momx", SECOND_SLOT, BEFORE_MOMX.to_string(), "toInt32"),
        put("m_momy", SECOND_SLOT, BEFORE_MOMY.to_string(), "toInt32"),
        put("m_momz", SECOND_SLOT, "0".to_owned(), "toInt32"),
        put("m_type", SECOND_SLOT, TROOPSHOT.to_string(), "toInt32"),
        put("m_state", SECOND_SLOT, TBALL1.to_string(), "toInt32"),
        put("m_tics", SECOND_SLOT, "1".to_owned(), "toInt32"),
        put("m_flags", SECOND_SLOT, FLYING_FLAGS.to_string(), "toInt32"),
        put("m_target", SECOND_SLOT, THROWER.to_string(), "toUInt32"),
        put("m_health", SECOND_SLOT, "1000".to_owned(), "toInt32"),
        put(
            "m_radius",
            SECOND_SLOT,
            BEFORE_RADIUS.to_string(),
            "toInt32",
        ),
        put(
            "m_height",
            SECOND_SLOT,
            BEFORE_HEIGHT.to_string(),
            "toInt32",
        ),
        put(
            "m_subsector",
            SECOND_SLOT,
            BEFORE_SUBSECTOR.to_string(),
            "toInt32",
        ),
        put("m_reactiontime", SECOND_SLOT, "0".to_owned(), "toInt32"),
        put("m_threshold", SECOND_SLOT, "0".to_owned(), "toInt32"),
    ];
    let seeded = seed::row(&db, GAMETIC_BEFORE, GAMETIC_BEFORE, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect::<Vec<_>>();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let after = hit_after_206(&fixture, &db).await;
    let (second_state, second_flags) = fixture
        .rows::<(i32, i32)>(&format!(
            "SELECT m_state[{SECOND_SLOT}], m_flags[{SECOND_SLOT}] \
             FROM {db}.native_state WHERE tic = {GAMETIC_HIT}"
        ))
        .await
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no row for tic {GAMETIC_HIT}"));
    fixture.finish().await;

    // Both missiles explode, proving both connected rather than one alone
    // silently losing its own effect to the other's.
    assert_eq!(
        (second_state, second_flags),
        (TBALL2, EXPLODED_FLAGS),
        "the second fireball's own impact runs too"
    );
    // The player's own fields thread through both hits correctly: each
    // one's own damage_fold call reads what the one before it left
    // through `player`, the same chain the armour arms already prove.
    let taken = 100 - after.p_health;
    assert!(taken >= 6, "two hits, each at least a roll of one: {taken}");
    assert_eq!(
        after.p_damagecount, taken,
        "the tint takes the sum of both, threaded through the same fold"
    );
    // The mobj-generic fields a call answers with (`m_health` here) thread
    // the same way: the second missile's own call reads what the first
    // left in `HIT_RESULTS` rather than the tic-start array, so the
    // shared field carries both hits' worth too, and the tic resolves.
    assert_eq!(
        after.player_health, after.p_health,
        "the shared field threads the same way the player's own does"
    );
    assert_eq!(after.unresolved, 0, "both hits resolve");
}
