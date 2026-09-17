//! `A_TroopAttack` reached through a tic, against a real ClickHouse
//! server.
//!
//! `native/tests/sim_claw_live.rs` reads the routine itself against a
//! reader written from `p_enemy.c`. This reads what a tic does with it:
//! the frame the state cycle enters, the angle and the flags it leaves on
//! the attacker, and the damage that reaches the target.
//!
//! `demo3` reaches the routine once and the imp throws a fireball, so the
//! claw is seeded: an imp put beside another imp, one tic from the frame
//! the routine sits on.
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

/// The imp that runs the routine and the imp it swings at. Both are
/// `MT_TROOP` on the level's own list, and at gametic 40 both stand
/// still.
const ATTACKER: usize = 116;
const TARGET: usize = 117;

/// `states.tsv`: the frame carrying `A_FaceTarget`, and the frame after it
/// carrying `A_TroopAttack`. Seeding the first with one tic of wait left
/// puts the routine on the tic the arm runs.
const FACE: i32 = 453;
const ATTACK: i32 = 454;

/// `p_local.h`
const BASETHRESHOLD: i32 = 100;

/// `mobjtype.tsv`: the fireball an imp throws.
const TROOPSHOT: i32 = 31;

/// `p_mobj.h`
const MF_AMBUSH: i64 = 32;

/// The angle `R_PointToAngle2` gives for a target due west. The octant it
/// lands in counts down from half a turn, so the answer is a unit short of
/// it.
const DUE_WEST: u32 = 0x7fff_ffff;

/// How far from its target each arm stands the attacker. `MELEERANGE` is
/// sixty four units and `P_CheckMeleeRange` measures against that less
/// twenty, plus the target's radius. Four units is inside the claw's
/// reach and close enough that the two share a sector whatever the map
/// looks like there; four hundred is outside it.
const NEAR: i64 = 4 * 65536;
const FAR: i64 = 400 * 65536;

/// A third thing, standing between the attacker and its target, for the
/// arm where the fireball reaches something on the tic it is thrown. An
/// imp on the level's own list, turned into a zombieman so the same
/// species rule in `PIT_CheckThing` does not hold the damage back.
const BYSTANDER: usize = 118;

/// `mobjtype.tsv`, and `mobjinfo.tsv`'s own flags, radius and height for
/// it: `MF_SOLID | MF_SHOOTABLE | MF_COUNTKILL`, twenty units across and
/// fifty six tall. The height is what puts it in the fireball's way:
/// `PIT_CheckThing` lets a missile over a thing whose top is below it, and
/// `P_SpawnMissile` starts the fireball thirty two units up.
const MT_POSSESSED: i32 = 1;
const POSSESSED_FLAGS: i64 = 4_194_310;
const POSSESSED_RADIUS: i64 = 1_310_720;
const POSSESSED_HEIGHT: i64 = 3_670_016;
/// `mobjtype.tsv`: what a zombieman's death drops.
const MT_CLIP: i32 = 63;

/// How far along the shot the bystander stands.
///
/// `P_SpawnMissile` puts the fireball at the shooter and `P_CheckMissileSpawn`
/// steps it half its own momentum; `P_MobjThinker` then moves it a whole
/// one in the same tic. `MT_TROOPSHOT` moves ten units a tic, so the two
/// stops are five and fifteen units out. `PIT_CheckThing` calls it a hit
/// inside the two radii added together, twenty six units here, so thirty
/// five units clears the half step by four and is inside the whole one by
/// six.
const ALONG: i64 = 35 * 65536;

/// One arm per seeded row: its name, where the copy of `BEFORE` lands and
/// how far the attacker stands from its target. The tics are far apart so
/// the arms cannot read each other's rows.
///
/// `ambush` stands where `claw` stands and differs from it by one flag, so
/// what it reads is the flag and nothing else. `thrown_hit` stands where
/// `fireball` stands and adds the bystander.
const ARMS: [(&str, u32, i64); 4] = [
    ("claw", 200, NEAR),
    ("fireball", 300, FAR),
    ("ambush", 400, NEAR),
    ("thrown_hit", 500, FAR),
];

#[derive(Row, Deserialize)]
struct Clawed {
    tic: u32,
    state: i32,
    angle: u32,
    attacker_flags: i32,
    health: i32,
    hunts: u32,
    threshold: i32,
    prndindex: u8,
    unresolved: u64,
    things: u64,
    last_type: i32,
    last_target: u32,
    target_state: i32,
    target_sprite: i32,
    target_frame: i32,
    bystander_state: i32,
    bystander_sprite: i32,
    bystander_frame: i32,
    bystander_health: i32,
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

/// Two slots overridden in the same column. `seed::row` refuses a column
/// named twice, so a column both slots touch needs one combined entry.
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

#[tokio::test]
async fn a_tic_carries_the_imps_attack_through() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_troop").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let walk: Vec<Input> = (1..=BEFORE).map(Input::demo).collect();
    support::resident::run(&fixture, &walk, false).await;

    let mut statements: Vec<sql::Statement> = Vec::new();
    for (name, at, away) in ARMS {
        let flags = if name == "ambush" { MF_AMBUSH } else { 0 };
        let stands_by = name == "thrown_hit";
        let attacker_x = format!("p.m_x[{TARGET}] + {away}");
        let attacker_flags = format!("bitOr(toInt64(p.m_flags[{ATTACKER}]), {flags})");
        let of_target = |column: &str| format!("p.{column}[{TARGET}]");
        // The attacker beside its target, in the frame whose next carries
        // the routine, with one tic of wait left on it.
        let mut overrides = vec![
            put("m_state", ATTACKER, FACE.to_string(), "toInt32"),
            put("m_tics", ATTACKER, "1".to_owned(), "toInt32"),
            put("m_target", ATTACKER, TARGET.to_string(), "toUInt32"),
            // Nothing else has this one's attention, so what the claw
            // leaves is the whole of what moves its pointer.
            put("m_threshold", TARGET, "0".to_owned(), "toInt32"),
        ];
        match stands_by {
            // The bystander stands `ALONG` the shot from the attacker, on
            // the target's own floor. A zombieman rather than a third imp,
            // because `PIT_CheckThing` lets a missile through a thing of
            // its own shooter's type without damaging it, and at one point
            // of health, so the fireball's own roll always kills.
            true => overrides.extend([
                put2(
                    "m_x",
                    ATTACKER,
                    attacker_x.clone(),
                    BYSTANDER,
                    format!("({attacker_x}) - {ALONG}"),
                    "toInt32",
                ),
                put2(
                    "m_y",
                    ATTACKER,
                    of_target("m_y"),
                    BYSTANDER,
                    of_target("m_y"),
                    "toInt32",
                ),
                put2(
                    "m_z",
                    ATTACKER,
                    of_target("m_z"),
                    BYSTANDER,
                    of_target("m_z"),
                    "toInt32",
                ),
                put2(
                    "m_flags",
                    ATTACKER,
                    attacker_flags,
                    BYSTANDER,
                    POSSESSED_FLAGS.to_string(),
                    "toInt32",
                ),
                put("m_floorz", BYSTANDER, of_target("m_floorz"), "toInt32"),
                put("m_ceilingz", BYSTANDER, of_target("m_ceilingz"), "toInt32"),
                put(
                    "m_subsector",
                    BYSTANDER,
                    of_target("m_subsector"),
                    "toInt32",
                ),
                put("m_type", BYSTANDER, MT_POSSESSED.to_string(), "toInt32"),
                put(
                    "m_radius",
                    BYSTANDER,
                    POSSESSED_RADIUS.to_string(),
                    "toInt32",
                ),
                put(
                    "m_height",
                    BYSTANDER,
                    POSSESSED_HEIGHT.to_string(),
                    "toInt32",
                ),
                put("m_health", BYSTANDER, "1".to_owned(), "toInt32"),
            ]),
            false => overrides.extend([
                put("m_x", ATTACKER, attacker_x, "toInt32"),
                put("m_y", ATTACKER, of_target("m_y"), "toInt32"),
                put("m_z", ATTACKER, of_target("m_z"), "toInt32"),
                put("m_flags", ATTACKER, attacker_flags, "toInt32"),
            ]),
        }
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
    let rows: Vec<Clawed> = fixture
        .rows(&format!(
            "SELECT tic, m_state[{ATTACKER}] AS state, m_angle[{ATTACKER}] AS angle, \
             m_flags[{ATTACKER}] AS attacker_flags, m_health[{TARGET}] AS health, \
             m_target[{TARGET}] AS hunts, m_threshold[{TARGET}] AS threshold, \
             prndindex, unresolved, toUInt64(length(m_x)) AS things, \
             m_type[length(m_type)] AS last_type, \
             m_target[length(m_target)] AS last_target, \
             m_state[{TARGET}] AS target_state, m_sprite[{TARGET}] AS target_sprite, \
             m_frame[{TARGET}] AS target_frame, \
             m_state[{BYSTANDER}] AS bystander_state, \
             m_sprite[{BYSTANDER}] AS bystander_sprite, \
             m_frame[{BYSTANDER}] AS bystander_frame, \
             m_health[{BYSTANDER}] AS bystander_health \
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

    // The imp within reach claws its target.
    let (before, after) = (at(200), at(201));
    assert_eq!(
        before.state, FACE,
        "the seeded row is a tic from the routine"
    );
    assert_eq!(after.state, ATTACK, "and the cycle reaches it");
    // The target stands in its spawnstate and the pain roll misses, so
    // `P_DamageMobj` wakes it with `P_SetMobjState(seestate)`, which runs
    // `A_Chase` inside the damage call. `damaged` does not run that, so
    // the tic is unresolved even though the claw's own effect is exact.
    assert_eq!(
        after.unresolved,
        sim::unresolved::DM_STUCK,
        "the claw wakes its target into a chase"
    );
    let taken = before.health - after.health;
    assert!(
        (3..=24).contains(&taken) && taken % 3 == 0,
        "the damage is three times one to eight: {taken}"
    );
    assert_ne!(
        after.prndindex, before.prndindex,
        "and the damage draws for itself"
    );
    // `A_FaceTarget` runs first, and the target stands four units back
    // along the x axis, so the attacker ends up pointing straight down
    // it.
    assert_eq!(after.angle, DUE_WEST, "the attacker turns onto its target");
    // `P_DamageMobj` turns a thing with nothing else on its mind onto
    // whatever hit it, which is how the claw is told apart from a hit
    // that only moved a number.
    assert_eq!(
        after.hunts, ATTACKER as u32,
        "the target turns on the imp that clawed it"
    );
    assert_eq!(
        after.threshold, BASETHRESHOLD,
        "and holds that for the threshold's worth of tics"
    );
    // `P_SetMobjState` writes the picture with the state, so the frame the
    // wake put the target into is the frame it shows.
    assert_ne!(
        after.target_state, before.target_state,
        "the claw moves the target out of its spawn frame"
    );
    assert_eq!(
        (after.target_sprite as i64, after.target_frame as i64),
        support::damage::picture(after.target_state as i64),
        "the target's picture follows the state the claw put it in"
    );

    // The imp out of reach throws a fireball, which goes on the end of the
    // list.
    let (before, after) = (at(300), at(301));
    assert_eq!(after.state, ATTACK, "the cycle reaches the routine");
    assert_eq!(
        after.health, before.health,
        "no claw reaches a target four hundred units away"
    );
    assert_eq!(
        after.things,
        before.things + 1,
        "the throw puts one more thing on the list"
    );
    assert_eq!(
        after.last_type, TROOPSHOT,
        "and the thing it puts there is a fireball"
    );
    assert_eq!(
        after.last_target, ATTACKER as u32,
        "carrying a pointer back at the imp that threw it"
    );
    assert_ne!(
        after.prndindex, before.prndindex,
        "and the spawn draws for itself"
    );

    // `A_FaceTarget` takes the thing off ambush, and the tic carries that
    // through with the rest of what the routine leaves.
    let (before, after) = (at(400), at(401));
    assert_eq!(
        before.attacker_flags & MF_AMBUSH as i32,
        MF_AMBUSH as i32,
        "the seeded row puts the attacker on ambush"
    );
    assert_eq!(
        after.attacker_flags & MF_AMBUSH as i32,
        0,
        "and the routine takes it off"
    );
    // The same wake-into-chase the near arm hits: the target's pain roll
    // misses and `P_DamageMobj` runs `A_Chase` for it via `P_SetMobjState`.
    assert_eq!(
        after.unresolved,
        sim::unresolved::DM_STUCK,
        "the claw wakes its target into a chase"
    );
    assert!(before.health - after.health > 0, "and the claw still lands");
    assert_eq!(
        (after.target_sprite as i64, after.target_frame as i64),
        support::damage::picture(after.target_state as i64),
        "and the target's picture follows the state it put it in"
    );

    // The fireball reaches the bystander on the tic it is thrown, which is
    // the thrown missile's own thinker rather than the impact of one
    // already on the list.
    let (before, after) = (at(500), at(501));
    assert_eq!(before.bystander_health, 1, "the seeded row stands it up");
    assert_eq!(
        after.unresolved & sim::unresolved::TK_STUCK,
        0,
        "the hit on the tic the fireball is thrown is a path this runs"
    );
    assert!(
        after.bystander_health <= 0,
        "the fireball's own damage kills it: {}",
        after.bystander_health
    );
    assert_eq!(
        after.things,
        before.things + 2,
        "the throw and the kill's own drop each put one more thing on the list"
    );
    assert_eq!(
        after.last_type, MT_CLIP,
        "and the last of them is the clip the zombieman dropped"
    );
    assert_ne!(
        after.bystander_state, before.bystander_state,
        "the kill moves it into its death frames"
    );
    assert_eq!(
        (after.bystander_sprite as i64, after.bystander_frame as i64),
        support::damage::picture(after.bystander_state as i64),
        "and its picture follows that state"
    );
}
