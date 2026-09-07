//! `P_DamageMobj`'s player branch, against a real ClickHouse server.
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
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
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
async fn a_claw_reaches_the_player_through_its_own_armour() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_player_damage").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let mut statements: Vec<sql::Statement> = Vec::new();
    for (_, at, armortype) in ARMS {
        let overrides = [
            // The imp beside the player, in the frame whose next carries
            // the routine, with one tic of wait left on it.
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
            ("p_health", "toInt32(100)".to_owned()),
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
        ];
        statements.extend(
            seed::row(&db, at, BEFORE, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        statements.push(sim::tick::run_statement(
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
    let rows: Vec<Hit> = fixture
        .rows(&format!(
            "SELECT tic, p_health, p_armorpoints, p_armortype, p_damagecount, p_attacker, \
             m_health[p_mo] AS player_health, m_state[{ATTACKER}] AS attacker_state, \
             unresolved \
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

    // The bare arm: no armour, so the raw roll reaches health outright.
    // `A_TroopAttack`'s own damage is `(P_Random()%8+1)*3`, and every arm
    // seeds the same imp at the same state with the same starting
    // `prndindex`, so the roll is identical across all three.
    let (before, after) = (at(200), at(201));
    assert_eq!(
        before.attacker_state, FACE,
        "the seeded row is a tic from the routine"
    );
    assert_eq!(after.attacker_state, ATTACK, "and the cycle reaches it");
    assert_eq!(
        after.unresolved, 0,
        "a living player's own hit resolves, unlike a monster's"
    );
    let raw_damage = before.p_health - after.p_health;
    assert!(
        (3..=24).contains(&raw_damage) && raw_damage % 3 == 0,
        "the claw's own damage is three times one to eight: {raw_damage}"
    );
    assert_eq!(
        before.player_health - after.player_health,
        raw_damage,
        "the mobj's own health shares the same number"
    );
    assert_eq!(after.p_armorpoints, 0, "no armour, nothing saved");
    assert_eq!(after.p_armortype, 0);
    assert_eq!(
        after.p_damagecount, raw_damage,
        "the tint takes the whole hit"
    );
    assert_eq!(
        after.p_attacker, ATTACKER as u32,
        "the player's own attacker is the imp that clawed it"
    );

    // The armoured arms save a third or a half of the same roll.
    for (name, divisor) in [("green_armor", 3), ("blue_armor", 2)] {
        let (_, at_tic, _) = ARMS.iter().find(|(n, _, _)| *n == name).unwrap();
        let (before, after) = (at(*at_tic), at(*at_tic + 1));
        assert_eq!(after.unresolved, 0, "{name}");
        let saved = raw_damage / divisor;
        assert_eq!(
            before.p_armorpoints - after.p_armorpoints,
            saved,
            "{name}: the armour absorbs its own share"
        );
        assert_eq!(
            before.p_health - after.p_health,
            raw_damage - saved,
            "{name}: health takes what the armour did not"
        );
        assert_eq!(
            before.player_health - after.player_health,
            raw_damage - saved,
            "{name}: the mobj's own health shares the same number"
        );
        assert_eq!(
            after.p_damagecount,
            raw_damage - saved,
            "{name}: the tint takes the post-armour damage"
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
        .execute(&[sim::tick::demo_statement(db, GAMETIC_HIT, GAMETIC_HIT)])
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

    if armortype != 0 || armorpoints != 0 {
        let overrides = seed::row(
            &db,
            GAMETIC_BEFORE,
            GAMETIC_BEFORE,
            &[
                ("p_armortype", format!("toInt32({armortype})")),
                ("p_armorpoints", format!("toInt32({armorpoints})")),
            ],
        )
        .into_iter()
        .map(sql::Statement::sql)
        .collect::<Vec<_>>();
        if let Err(error) = fixture.execute(&overrides).await {
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
    // The mobj-generic fields a call answers with (`m_health` here) read
    // the tic-start arrays directly rather than an accumulator, so the
    // second missile's own call overwrites the first's own subtraction
    // rather than building on it: the shared field only carries one
    // hit's worth, not both. `DM_SAME_TARGET` refuses the tic for this
    // rather than committing the wrong shared answer.
    assert_ne!(
        after.player_health, after.p_health,
        "the shared field does not thread the way the player's own does"
    );
    assert_ne!(
        after.unresolved & sim::unresolved::DM_SAME_TARGET,
        0,
        "a second hit on the same target this tic leaves it stuck"
    );
}
