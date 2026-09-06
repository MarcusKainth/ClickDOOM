//! `A_TroopAttack` and `A_SargAttack` against a real ClickHouse server.
//!
//! `demo3` reaches neither before the first divergence, so the world is
//! seeded: an imp and a demon facing targets in reach, out of reach, and
//! out of sight, and one of them facing nothing at all. Every answer is
//! compared against `native/tests/support/attacks.rs`, a reader written
//! from `p_enemy.c`.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{self, attacks};
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::attacks::{Attacked, Fighter, Routine, World as Oracle};
use support::db::Fixture;
use support::mobj::thing_type;

const FRACUNIT: i64 = 1 << 16;
/// `p_mobj.h`
const MF_SOLID: i64 = 2;
const MF_SHOOTABLE: i64 = 4;
const MF_AMBUSH: i64 = 32;
const MF_SHADOW: i64 = 0x4_0000;

/// The random indices the fan attacks from, and the draw offsets it
/// attacks at.
const INDICES: [i64; 4] = [0, 61, 137, 253];
const BASES: [i64; 3] = [0, 5, 17];

/// `mobjinfo`'s own number for a type, by name.
fn info(kind: &str, column: &str) -> i64 {
    tables::table("mobjinfo").unwrap().ints(column).unwrap()[thing_type(kind) as usize]
}

/// The `action_functions` id the engine's own table gives a routine.
fn action(name: &str) -> i64 {
    let actions = tables::table("action_functions").unwrap();
    let at = actions
        .texts("name")
        .unwrap()
        .iter()
        .position(|held| *held == name)
        .expect("the engine carries the routine");
    actions.ints("id").unwrap()[at]
}

/// One case: the attacker, its routine, whether it can see its target, and
/// what the case is called.
struct Case {
    what: &'static str,
    slot: usize,
    routine: Routine,
    sees: bool,
    /// Whether it has a target at all. One that has none returns before
    /// `A_FaceTarget` and leaves every flag where it was.
    has_target: bool,
}

#[tokio::test]
async fn an_attack_leaves_what_the_engine_leaves() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_claw").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let (fighters, cases) = seeded();
    for prnd in INDICES {
        for base in BASES {
            let ours = ask_server(&fixture, &db, &fighters, &cases, prnd, base).await;
            let oracle = Oracle {
                fighters: fighters.clone(),
                prndindex: prnd,
            };
            for (at, case) in cases.iter().enumerate() {
                let want = oracle.attack(case.slot, case.routine, case.sees, base);
                assert_eq!(ours[at].0, want, "index {prnd}, base {base}: {}", case.what);
                assert_eq!(ours[at].1, 0, "{} reaches no unwritten path", case.what);
            }
            check(prnd, &ours, &cases);
        }
    }

    a_routine_it_does_not_write_leaves_the_call_stuck(&fixture, &db, &fighters, cases[0].slot)
        .await;
    fixture.finish().await;
}

/// That the fan reached every arm: a claw that lands, one held off by the
/// distance, one held off by the sight, a fireball, and an attacker with
/// no target at all.
fn check(prnd: i64, ours: &[(Attacked, u8)], cases: &[Case]) {
    let clawed = ours.iter().filter(|a| a.0.clawed).count();
    let threw = ours.iter().filter(|a| a.0.throws).count();
    let quiet = ours.iter().filter(|a| a.0.draws == 0).count();
    let fuzzed = ours.iter().filter(|a| a.0.draws >= 2).count();
    assert!(
        clawed > 0 && threw > 0 && quiet > 0 && fuzzed > 0,
        "index {prnd} reaches every arm: clawed {clawed}, threw {threw}, \
         quiet {quiet}, fuzzed {fuzzed}"
    );
    // A thing that reaches `A_FaceTarget` stops being an ambusher,
    // whatever else it does. One with no target never reaches it.
    for (at, case) in cases.iter().enumerate() {
        let ambushing = ours[at].0.flags & MF_AMBUSH != 0;
        assert_eq!(
            ambushing, !case.has_target,
            "{} and the ambush flag",
            case.what
        );
    }
}

/// A frame carrying a routine this does not write leaves the call stuck.
async fn a_routine_it_does_not_write_leaves_the_call_stuck(
    fixture: &Fixture,
    db: &str,
    fighters: &[Fighter],
    slot: usize,
) {
    let cases = [Case {
        what: "a routine the generator does not write",
        slot,
        routine: Routine::Troop,
        sees: true,
        has_target: true,
    }];
    let ours = ask_with(fixture, db, fighters, &cases, 0, 0, action("A_CyberAttack")).await;
    assert_eq!(ours[0].1, 1, "the call says it could not be run");
}

/// The seeded world and the cases run over it.
///
/// The attackers stand at the origin and the targets due east of them, so
/// the claw's reach is the one number each case turns on.
fn seeded() -> (Vec<Fighter>, Vec<Case>) {
    let mut fighters: Vec<Fighter> = Vec::new();
    let mut put = |x: i64, kind: i64, flags: i64, target: usize| {
        fighters.push(Fighter {
            x,
            y: 0,
            angle: 0,
            kind,
            flags,
            target,
        });
        fighters.len()
    };
    let troop = thing_type("MT_TROOP");
    let sarg = thing_type("MT_SERGEANT");
    let solid = MF_SOLID | MF_SHOOTABLE;
    // The targets: one within a claw's reach, one well outside it, and one
    // in reach that a shadow makes the face draw for.
    let near = put(40 * FRACUNIT, troop, solid, 0);
    let far = put(400 * FRACUNIT, troop, solid, 0);
    let fuzzy = put(40 * FRACUNIT, troop, solid | MF_SHADOW, 0);
    // Between `MELEERANGE` less the slop plus the target's radius, which
    // is 64 units for an imp, and `MELEERANGE` plus it, which is 84. Only
    // a reach that takes the slop off holds this one out.
    let edge = put(70 * FRACUNIT, troop, solid, 0);

    let ambush = |kind: i64| info_flags(kind) | MF_AMBUSH;
    let cases = vec![
        (
            "an imp clawing what it can reach",
            troop,
            near,
            Routine::Troop,
            true,
        ),
        ("an imp too far to claw", troop, far, Routine::Troop, true),
        (
            "an imp in reach it cannot see",
            troop,
            near,
            Routine::Troop,
            false,
        ),
        ("an imp facing a shadow", troop, fuzzy, Routine::Troop, true),
        (
            "an imp just outside its reach",
            troop,
            edge,
            Routine::Troop,
            true,
        ),
        ("an imp with no target", troop, 0, Routine::Troop, true),
        (
            "a demon clawing what it can reach",
            sarg,
            near,
            Routine::Sarg,
            true,
        ),
        ("a demon too far to claw", sarg, far, Routine::Sarg, true),
        ("a demon facing a shadow", sarg, fuzzy, Routine::Sarg, true),
        (
            "a demon just outside its reach",
            sarg,
            edge,
            Routine::Sarg,
            true,
        ),
        ("a demon with no target", sarg, 0, Routine::Sarg, true),
    ];
    let cases: Vec<Case> = cases
        .into_iter()
        .map(|(what, kind, target, routine, sees)| Case {
            what,
            slot: put(0, kind, ambush(kind), target),
            routine,
            sees,
            has_target: target != 0,
        })
        .collect();
    (fighters, cases)
}

/// `mobjinfo`'s own flags for a type by its id.
fn info_flags(kind: i64) -> i64 {
    tables::table("mobjinfo").unwrap().ints("flags").unwrap()[kind as usize]
}

async fn ask_server(
    fixture: &Fixture,
    db: &str,
    fighters: &[Fighter],
    cases: &[Case],
    prnd: i64,
    base: i64,
) -> Vec<(Attacked, u8)> {
    ask_with(fixture, db, fighters, cases, prnd, base, -1).await
}

/// The attack over every case. `routine` overrides the one the case names,
/// which is how the unwritten-routine arm is reached.
async fn ask_with(
    fixture: &Fixture,
    db: &str,
    fighters: &[Fighter],
    cases: &[Case],
    prnd: i64,
    base: i64,
    routine: i64,
) -> Vec<(Attacked, u8)> {
    let of = |get: &dyn Fn(&Fighter) -> i64| {
        format!(
            "[{}]",
            fighters
                .iter()
                .map(|f| get(f).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let (m_x, m_y, m_angle) = (of(&|f| f.x), of(&|f| f.y), of(&|f| f.angle));
    let (m_flags, m_type) = (of(&|f| f.flags), of(&|f| f.kind));
    let m_target = of(&|f| f.target as i64);
    // The claw reads none of these; the gunshot beside it does.
    let m_z = of(&|_| 0);
    let m_height = of(&|_| 56 * FRACUNIT);
    let m_health = of(&|_| 100);
    let prndindex = prnd.to_string();
    let world = attacks::Attacking {
        m_x: &m_x,
        m_y: &m_y,
        m_z: &m_z,
        m_angle: &m_angle,
        m_height: &m_height,
        m_flags: &m_flags,
        m_type: &m_type,
        m_health: &m_health,
        m_target: &m_target,
        prndindex: &prndindex,
    };
    let named = |case: &Case| match case.routine {
        Routine::Troop => action("A_TroopAttack"),
        Routine::Sarg => action("A_SargAttack"),
    };
    let list = format!(
        "[{}]",
        cases
            .iter()
            .map(|case| format!(
                "(toUInt32({}), toInt32({}), toUInt8({}), toUInt32({base}))",
                case.slot,
                if routine == -1 { named(case) } else { routine },
                u8::from(case.sees),
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    #[derive(Row, Deserialize)]
    struct Struck {
        angle: Vec<u32>,
        flags: Vec<i32>,
        clawed: Vec<u8>,
        damage: Vec<i32>,
        throws: Vec<u8>,
        draws: Vec<u32>,
        stuck: Vec<u8>,
    }
    let column = |name: &str, field: usize| format!("arrayMap(t -> t.{field}, ak) AS {name}");
    let sql = format!(
        "SELECT\n    {}\nFROM\n(\n    WITH\n{},\n    ({list}) AS ak_asks\n    \
         SELECT {} AS ak\n)",
        [
            column("angle", attacks::attacked::ANGLE),
            column("flags", attacks::attacked::FLAGS),
            column("clawed", attacks::attacked::CLAWED),
            column("damage", attacks::attacked::DAMAGE),
            column("throws", attacks::attacked::THROWS),
            column("draws", attacks::attacked::DRAWS),
            column("stuck", attacks::attacked::STUCK),
        ]
        .join(",\n    "),
        sim::constants(db)
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        attacks::attack("ak_asks", &world),
    );
    let ours: Struck = fixture.scalar(&sql).await;
    let _ = info("MT_TROOP", "radius");
    (0..ours.angle.len())
        .map(|at| {
            (
                Attacked {
                    angle: i64::from(ours.angle[at]),
                    flags: i64::from(ours.flags[at]),
                    clawed: ours.clawed[at] == 1,
                    damage: i64::from(ours.damage[at]),
                    throws: ours.throws[at] == 1,
                    draws: i64::from(ours.draws[at]),
                },
                ours.stuck[at],
            )
        })
        .collect()
}
