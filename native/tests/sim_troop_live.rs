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

/// One arm per seeded row: its name, where the copy of `BEFORE` lands and
/// how far the attacker stands from its target. The tics are far apart so
/// the arms cannot read each other's rows.
///
/// `ambush` stands where `claw` stands and differs from it by one flag, so
/// what it reads is the flag and nothing else.
const ARMS: [(&str, u32, i64); 3] = [
    ("claw", 200, NEAR),
    ("fireball", 300, FAR),
    ("ambush", 400, NEAR),
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
    unresolved: u8,
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
async fn a_tic_carries_the_imps_attack_through() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_troop").await;
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
    for (name, at, away) in ARMS {
        let flags = if name == "ambush" { MF_AMBUSH } else { 0 };
        let overrides = [
            // The attacker beside its target, in the frame whose next
            // carries the routine, with one tic of wait left on it.
            put(
                "m_x",
                ATTACKER,
                format!("p.m_x[{TARGET}] + {away}"),
                "toInt32",
            ),
            put("m_y", ATTACKER, format!("p.m_y[{TARGET}]"), "toInt32"),
            put("m_z", ATTACKER, format!("p.m_z[{TARGET}]"), "toInt32"),
            put("m_state", ATTACKER, FACE.to_string(), "toInt32"),
            put("m_tics", ATTACKER, "1".to_owned(), "toInt32"),
            put("m_target", ATTACKER, TARGET.to_string(), "toUInt32"),
            put(
                "m_flags",
                ATTACKER,
                format!("bitOr(toInt64(p.m_flags[{ATTACKER}]), {flags})"),
                "toInt32",
            ),
            // Nothing else has this one's attention, so what the claw
            // leaves is the whole of what moves its pointer.
            put("m_threshold", TARGET, "0".to_owned(), "toInt32"),
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
    let rows: Vec<Clawed> = fixture
        .rows(&format!(
            "SELECT tic, m_state[{ATTACKER}] AS state, m_angle[{ATTACKER}] AS angle, \
             m_flags[{ATTACKER}] AS attacker_flags, m_health[{TARGET}] AS health, \
             m_target[{TARGET}] AS hunts, m_threshold[{TARGET}] AS threshold, \
             prndindex, unresolved \
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
    assert_eq!(after.unresolved, 0, "the claw is a branch this runs");
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

    // The imp out of reach throws a fireball, which this tic does not
    // spawn.
    let (before, after) = (at(300), at(301));
    assert_eq!(after.state, ATTACK, "the cycle reaches the routine");
    assert_eq!(
        after.health, before.health,
        "nothing reaches a target four hundred units away"
    );
    assert_eq!(
        after.unresolved, 1,
        "and the fireball says the tic could not be produced"
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
    assert_eq!(after.unresolved, 0, "on a tic that runs");
    assert!(before.health - after.health > 0, "and the claw still lands");
}
