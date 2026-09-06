//! `A_PosAttack` and `A_SPosAttack` against a real ClickHouse server.
//!
//! `demo3` reaches the zombieman's shot at gametic 300 and the shotgun
//! guy's at 612, both past the first divergence, so the world is seeded on
//! top of the level's own: `E1M7`'s things stand where `P_SetupLevel` left
//! them and a few of them are pointed at the player.
//!
//! Every number the routines work out is compared against
//! `native/tests/support/attacks.rs`, a reader written from `p_enemy.c`.
//! What each shot reaches comes from `P_LineAttack`'s own walk, which
//! `sim_hitscan_live` checks against its own reader; what this checks is
//! the face, the spread, the damage roll and the order the three run in.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{self, attacks, inter, shoot};
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::attacks::{Fighter, Gunned, World as Oracle};
use support::db::Fixture;
use support::mobj::thing_type;

/// `p_mobj.h`
const MF_SHADOW: i64 = 0x4_0000;

/// The random indices the fan fires from, and the draw offsets it fires
/// at.
const INDICES: [i64; 3] = [0, 61, 137];
const BASES: [i64; 2] = [0, 17];

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

/// One case: who fires, with what routine, and what the case is called.
struct Case {
    what: &'static str,
    slot: usize,
    routine: &'static str,
    shots: usize,
}

#[tokio::test]
async fn a_gunshot_leaves_what_the_engine_leaves() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_gunshot").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let (mut world, player) = level(&fixture, &db).await;
    let standing = standing(&fixture, &db).await;
    let health = &standing.health;
    let cases = seeded(&mut world, player);

    let mut coverage = Coverage::default();
    for shadow in [false, true] {
        world[player - 1].flags = if shadow {
            world[player - 1].flags | MF_SHADOW
        } else {
            world[player - 1].flags & !MF_SHADOW
        };
        for prnd in INDICES {
            for base in BASES {
                let ours = ask_server(&fixture, &db, &world, &cases, prnd, base).await;
                let oracle = Oracle {
                    fighters: world.clone(),
                    prndindex: prnd,
                };
                for (at, case) in cases.iter().enumerate() {
                    let shots = if world[case.slot - 1].target == 0 {
                        0
                    } else {
                        case.shots
                    };
                    assert_eq!(
                        ours[at].kinds.len(),
                        shots,
                        "{} fires what the routine fires",
                        case.what
                    );
                    let want = oracle.gunshot(
                        case.slot,
                        shots,
                        base,
                        &support::attacks::Answered {
                            kinds: &ours[at].kinds,
                            hurt: &ours[at].hurt,
                            slope: ours[at].gunned.slope,
                        },
                    );
                    assert_eq!(
                        ours[at].gunned, want,
                        "shadow {shadow}, index {prnd}, base {base}: {}",
                        case.what
                    );
                    // `P_ShootSpecialLine` is not written, and a shot that
                    // kills what a later shot of the same call would reach
                    // changes what that shot is told.
                    let crossed = ours[at].crossed.iter().any(|held| *held > 0);
                    let killed = (0..ours[at].kinds.len()).any(|shot| {
                        ours[at].kinds[shot] == 2
                            && shot + 1 < ours[at].kinds.len()
                            && health[(ours[at].ids[shot] - 1) as usize]
                                - ours[at].gunned.damage[shot]
                                <= 0
                    });
                    assert_eq!(
                        ours[at].stuck == 1,
                        crossed || killed,
                        "{} says what it could not answer for",
                        case.what
                    );
                    coverage.count(&ours[at]);
                }
                // Where each shot went, against the angles the reader
                // works out on its own. The answer carries no angle, so
                // this asks the same walk for the oracle's own and
                // compares what it reached.
                let angles: Vec<Shot> = cases
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| !ours[*at].kinds.is_empty())
                    .flat_map(|(at, case)| {
                        // The first shot's own numbers sit behind what
                        // `A_FaceTarget` drew, which is the three the shot
                        // itself draws before its puff.
                        let mut drawn = ours[at].gunned.spawn_base[0] - 3;
                        (0..ours[at].kinds.len())
                            .map(|shot| {
                                let angle = oracle.shot_angle(case.slot, base, drawn);
                                drawn = ours[at].gunned.spawn_base[shot]
                                    + if ours[at].kinds[shot] == 0 { 0 } else { 4 }
                                    + ours[at].hurt[shot];
                                Shot {
                                    slot: case.slot,
                                    angle,
                                    slope: i64::from(ours[at].gunned.slope),
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect();
                let walked = walk(&fixture, &db, &world, &standing, &angles).await;
                let mut reached: Vec<(i64, i64)> = Vec::new();
                for held in &ours {
                    for shot in 0..held.kinds.len() {
                        reached.push((held.kinds[shot], held.ids[shot]));
                    }
                }
                assert_eq!(
                    walked, reached,
                    "shadow {shadow}, index {prnd}, base {base}: where the shots went"
                );
            }
        }
    }
    fixture.finish().await;
    coverage.check();
}

/// That the fan reached every arm.
#[derive(Default)]
struct Coverage {
    nothing: usize,
    wall: usize,
    thing: usize,
    one: usize,
    three: usize,
    quiet: usize,
    fuzzed: usize,
}

impl Coverage {
    fn count(&mut self, ours: &Fired) {
        self.nothing += ours.kinds.iter().filter(|k| **k == 0).count();
        self.wall += ours.kinds.iter().filter(|k| **k == 1).count();
        self.thing += ours.kinds.iter().filter(|k| **k == 2).count();
        match ours.kinds.len() {
            0 => self.quiet += 1,
            1 => self.one += 1,
            _ => self.three += 1,
        }
        if ours.gunned.draws >= 2 && !ours.kinds.is_empty() {
            let least = 3 * ours.kinds.len() as i64;
            if ours.gunned.spawn_base.first() == Some(&5) || ours.gunned.draws > least + 8 {
                self.fuzzed += 1;
            }
        }
    }

    /// A shot that reaches nothing at all is not among them: the level is
    /// closed and `MISSILERANGE` is longer than any sight line on it, so
    /// every shot ends on a wall or on a thing.
    fn check(&self) {
        assert!(
            self.nothing == 0
                && self.wall > 0
                && self.thing > 0
                && self.one > 0
                && self.three > 0
                && self.quiet > 0
                && self.fuzzed > 0,
            "the fan reaches every arm: nothing {}, wall {}, thing {}, one shot {}, \
             three shots {}, no target {}, fuzzy {}",
            self.nothing,
            self.wall,
            self.thing,
            self.one,
            self.three,
            self.quiet,
            self.fuzzed
        );
    }
}

/// What every thing on the list stands at, which is what a shot leaves
/// from and what it has to take off to kill.
struct Standing {
    z: Vec<i64>,
    height: Vec<i64>,
    health: Vec<i64>,
}

async fn standing(fixture: &Fixture, db: &str) -> Standing {
    #[derive(Row, Deserialize)]
    struct Held {
        z: Vec<i32>,
        height: Vec<i32>,
        health: Vec<i32>,
    }
    let row: Held = fixture
        .scalar(&format!(
            "SELECT m_z AS z, m_height AS height, m_health AS health \
             FROM {db}.native_state WHERE tic = 0"
        ))
        .await;
    Standing {
        z: row.z.iter().map(|v| i64::from(*v)).collect(),
        height: row.height.iter().map(|v| i64::from(*v)).collect(),
        health: row.health.iter().map(|v| i64::from(*v)).collect(),
    }
}

/// The things `P_SetupLevel` left and the slot the player stands in.
async fn level(fixture: &Fixture, db: &str) -> (Vec<Fighter>, usize) {
    #[derive(Row, Deserialize)]
    struct Arrays {
        x: Vec<i32>,
        y: Vec<i32>,
        angle: Vec<u32>,
        kind: Vec<i32>,
        flags: Vec<i32>,
        player: Vec<u32>,
    }
    let row: Arrays = fixture
        .scalar(&format!(
            "SELECT m_x AS x, m_y AS y, m_angle AS angle, m_type AS kind, m_flags AS flags, \
             [p_mo] AS player FROM {db}.native_state WHERE tic = 0"
        ))
        .await;
    let world = (0..row.x.len())
        .map(|at| Fighter {
            x: i64::from(row.x[at]),
            y: i64::from(row.y[at]),
            angle: i64::from(row.angle[at]),
            kind: i64::from(row.kind[at]),
            flags: i64::from(row.flags[at]),
            target: 0,
        })
        .collect();
    (world, row.player[0] as usize)
}

/// The cases, and the targets the world needs for them.
///
/// The shooters are the level's own zombiemen and shotgun guys, pointed at
/// the player, plus one of each left with no target at all.
fn seeded(world: &mut [Fighter], player: usize) -> Vec<Case> {
    let of = |kind: &str, world: &[Fighter]| -> Vec<usize> {
        let want = thing_type(kind);
        (1..=world.len())
            .filter(|slot| world[slot - 1].kind == want)
            .collect()
    };
    let zombies = of("MT_POSSESSED", world);
    let guys = of("MT_SHOTGUY", world);
    assert!(
        zombies.len() > 3 && guys.len() > 3,
        "the level holds enough of each: {} and {}",
        zombies.len(),
        guys.len()
    );
    let mut cases: Vec<Case> = Vec::new();
    for slot in zombies.iter().take(3) {
        world[slot - 1].target = player;
        cases.push(Case {
            what: "a zombieman firing at the player",
            slot: *slot,
            routine: "A_PosAttack",
            shots: 1,
        });
    }
    for slot in guys.iter().take(3) {
        world[slot - 1].target = player;
        cases.push(Case {
            what: "a shotgun guy firing at the player",
            slot: *slot,
            routine: "A_SPosAttack",
            shots: 3,
        });
    }
    cases.push(Case {
        what: "a zombieman with no target",
        slot: zombies[3],
        routine: "A_PosAttack",
        shots: 1,
    });
    cases.push(Case {
        what: "a shotgun guy with no target",
        slot: guys[3],
        routine: "A_SPosAttack",
        shots: 3,
    });
    cases
}

/// One shot the reader worked the angle out for.
struct Shot {
    slot: usize,
    angle: i64,
    slope: i64,
}

/// `P_LineAttack` at each angle the reader worked out, as what it reached.
async fn walk(
    fixture: &Fixture,
    db: &str,
    world: &[Fighter],
    standing: &Standing,
    angles: &[Shot],
) -> Vec<(i64, i64)> {
    if angles.is_empty() {
        return Vec::new();
    }
    let arrays = Arrays::of(fixture, db, world).await;
    let list = format!(
        "[{}]",
        angles
            .iter()
            .map(|shot| shoot::shooting(
                &shot.slot.to_string(),
                &world[shot.slot - 1].x.to_string(),
                &world[shot.slot - 1].y.to_string(),
                &standing.z[shot.slot - 1].to_string(),
                &standing.height[shot.slot - 1].to_string(),
                &shot.angle.to_string(),
                &(32 * 64 * (1 << 16)).to_string(),
                &shot.slope.to_string(),
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    #[derive(Row, Deserialize)]
    struct Walked {
        kinds: Vec<u8>,
        ids: Vec<i32>,
    }
    let ours: Walked = fixture
        .scalar(&format!(
            "SELECT arrayMap(s -> toUInt8(s.{}), w) AS kinds, \
             arrayMap(s -> toInt32(s.{}), w) AS ids\nFROM\n(\n    WITH\n{},\n    \
             ({list}) AS w_asks\n    SELECT {} AS w\n)",
            shoot::reached::KIND,
            shoot::reached::ID,
            sim::constants(db)
                .into_iter()
                .map(|(name, expr)| format!("    ({expr}) AS {name}"))
                .collect::<Vec<_>>()
                .join(",\n"),
            shoot::traverse("w_asks", &arrays.targets()),
        ))
        .await;
    (0..ours.kinds.len())
        .map(|at| (i64::from(ours.kinds[at]), i64::from(ours.ids[at])))
        .collect()
}

/// One answer, as the parts the comparison reads.
struct Fired {
    gunned: Gunned,
    kinds: Vec<i64>,
    ids: Vec<i64>,
    /// How many special lines each shot crossed.
    crossed: Vec<u64>,
    hurt: Vec<i64>,
    stuck: u8,
}

fn literal(of: &[i64]) -> String {
    format!(
        "[{}]",
        of.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
    )
}

fn at_zero(db: &str, column: &str) -> String {
    format!("joinGet('{db}.native_state', '{column}', toUInt32(0))")
}

/// The array literals the primitive reads the world through.
struct Arrays {
    m_x: String,
    m_y: String,
    m_z: String,
    m_angle: String,
    m_radius: String,
    m_height: String,
    m_flags: String,
    m_type: String,
    m_health: String,
    m_target: String,
    m_linkseq: String,
    m_state: String,
    m_tics: String,
    m_threshold: String,
    m_player: String,
    alive: String,
    zero: String,
    floorheight: String,
    ceilingheight: String,
    line_special: String,
}

impl Arrays {
    async fn of(fixture: &Fixture, db: &str, world: &[Fighter]) -> Arrays {
        #[derive(Row, Deserialize)]
        struct Held {
            z: Vec<i32>,
            radius: Vec<i32>,
            height: Vec<i32>,
            health: Vec<i32>,
            linkseq: Vec<u32>,
            state: Vec<i32>,
            tics: Vec<i32>,
            threshold: Vec<i32>,
            player: Vec<i8>,
        }
        let row: Held = fixture
            .scalar(&format!(
                "SELECT m_z AS z, m_radius AS radius, m_height AS height, m_health AS health, \
                 m_linkseq AS linkseq, m_state AS state, m_tics AS tics, \
                 m_threshold AS threshold, m_player AS player \
                 FROM {db}.native_state WHERE tic = 0"
            ))
            .await;
        let of =
            |get: &dyn Fn(usize) -> i64| literal(&(0..world.len()).map(get).collect::<Vec<_>>());
        Arrays {
            m_x: of(&|at| world[at].x),
            m_y: of(&|at| world[at].y),
            m_z: of(&|at| i64::from(row.z[at])),
            m_angle: of(&|at| world[at].angle),
            m_radius: of(&|at| i64::from(row.radius[at])),
            m_height: of(&|at| i64::from(row.height[at])),
            m_flags: of(&|at| world[at].flags),
            m_type: of(&|at| world[at].kind),
            m_health: of(&|at| i64::from(row.health[at])),
            m_target: format!(
                "CAST({}, 'Array(UInt32)')",
                of(&|at| world[at].target as i64)
            ),
            m_linkseq: format!(
                "CAST({}, 'Array(UInt32)')",
                of(&|at| i64::from(row.linkseq[at]))
            ),
            m_state: of(&|at| i64::from(row.state[at])),
            m_tics: of(&|at| i64::from(row.tics[at])),
            m_threshold: of(&|at| i64::from(row.threshold[at])),
            m_player: of(&|at| i64::from(row.player[at])),
            alive: format!("CAST({}, 'Array(UInt8)')", of(&|_| 1)),
            zero: of(&|_| 0),
            floorheight: at_zero(db, "sec_floorheight"),
            ceilingheight: at_zero(db, "sec_ceilingheight"),
            line_special: at_zero(db, "line_special"),
        }
    }

    fn attacking<'a>(&'a self, prndindex: &'a str) -> attacks::Attacking<'a> {
        attacks::Attacking {
            m_x: &self.m_x,
            m_y: &self.m_y,
            m_z: &self.m_z,
            m_angle: &self.m_angle,
            m_height: &self.m_height,
            m_flags: &self.m_flags,
            m_type: &self.m_type,
            m_health: &self.m_health,
            m_target: &self.m_target,
            prndindex,
        }
    }

    fn targets(&self) -> shoot::Targets<'_> {
        shoot::Targets {
            m_x: &self.m_x,
            m_y: &self.m_y,
            m_z: &self.m_z,
            m_radius: &self.m_radius,
            m_height: &self.m_height,
            m_flags: &self.m_flags,
            m_linkseq: &self.m_linkseq,
            alive: &self.alive,
            floorheight: &self.floorheight,
            ceilingheight: &self.ceilingheight,
            line_special: &self.line_special,
        }
    }

    fn hurting<'a>(&'a self, prndindex: &'a str) -> inter::Hurting<'a> {
        inter::Hurting {
            m_x: &self.m_x,
            m_y: &self.m_y,
            m_z: &self.m_z,
            m_momx: &self.zero,
            m_momy: &self.zero,
            m_momz: &self.zero,
            m_reactiontime: &self.zero,
            m_type: &self.m_type,
            m_state: &self.m_state,
            m_tics: &self.m_tics,
            m_flags: &self.m_flags,
            m_health: &self.m_health,
            m_height: &self.m_height,
            m_target: &self.m_target,
            m_threshold: &self.m_threshold,
            m_player: &self.m_player,
            prndindex,
            readyweapon: "0",
        }
    }
}

async fn ask_server(
    fixture: &Fixture,
    db: &str,
    world: &[Fighter],
    cases: &[Case],
    prnd: i64,
    base: i64,
) -> Vec<Fired> {
    let arrays = Arrays::of(fixture, db, world).await;
    let prndindex = prnd.to_string();
    let list = format!(
        "[{}]",
        cases
            .iter()
            .map(|case| format!(
                "(toUInt32({}), toInt32({}), toUInt32({base}))",
                case.slot,
                action(case.routine)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    #[derive(Row, Deserialize)]
    struct Answer {
        angle: Vec<u32>,
        flags: Vec<i32>,
        damage: Vec<Vec<i32>>,
        spawn_base: Vec<Vec<u32>>,
        hurt_base: Vec<Vec<u32>>,
        draws: Vec<u32>,
        slope: Vec<i32>,
        stuck: Vec<u8>,
        kinds: Vec<Vec<u8>>,
        ids: Vec<Vec<i32>>,
        crossed: Vec<Vec<u64>>,
    }
    let column = |name: &str, field: usize| format!("arrayMap(t -> t.{field}, gn) AS {name}");
    let sql = format!(
        "SELECT\n    {},\n    arrayMap(t -> arrayMap(s -> toUInt8(s.{kind}), t.{shots}), gn) AS kinds,\
         \n    arrayMap(t -> arrayMap(s -> toInt32(s.{id}), t.{shots}), gn) AS ids,\
         \n    arrayMap(t -> arrayMap(s -> toUInt64(length(s.{spechit})), t.{shots}), gn) AS crossed\
         \nFROM\n(\n    WITH\n{},\n    ({list}) AS gn_asks\n    SELECT {} AS gn\n)",
        [
            column("angle", attacks::gunned::ANGLE),
            column("flags", attacks::gunned::FLAGS),
            column("damage", attacks::gunned::DAMAGE),
            column("spawn_base", attacks::gunned::SPAWN_BASE),
            column("hurt_base", attacks::gunned::HURT_BASE),
            column("draws", attacks::gunned::DRAWS),
            column("slope", attacks::gunned::SLOPE),
            column("stuck", attacks::gunned::STUCK),
        ]
        .join(",\n    "),
        sim::constants(db)
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        attacks::hitscan(
            "gn_asks",
            &arrays.attacking(&prndindex),
            &arrays.targets(),
            &arrays.hurting(&prndindex),
        ),
        kind = shoot::reached::KIND,
        id = shoot::reached::ID,
        spechit = shoot::reached::SPECHIT,
        shots = attacks::gunned::SHOTS,
    );
    let ours: Answer = fixture.scalar(&sql).await;
    (0..ours.angle.len())
        .map(|at| {
            let kinds: Vec<i64> = ours.kinds[at].iter().map(|k| i64::from(*k)).collect();
            let spawn: Vec<i64> = ours.spawn_base[at].iter().map(|b| i64::from(*b)).collect();
            let hurt_base: Vec<i64> = ours.hurt_base[at].iter().map(|b| i64::from(*b)).collect();
            // What each shot's damage call drew, which is the gap the
            // answer leaves between one shot's damage base and the next
            // shot's own numbers.
            let mut hurt: Vec<i64> = Vec::new();
            for shot in 0..kinds.len() {
                let after = if shot + 1 < kinds.len() {
                    spawn[shot + 1] - 3
                } else {
                    i64::from(ours.draws[at])
                };
                hurt.push(after - hurt_base[shot]);
            }
            Fired {
                ids: ours.ids[at].iter().map(|k| i64::from(*k)).collect(),
                crossed: ours.crossed[at].clone(),
                gunned: Gunned {
                    angle: i64::from(ours.angle[at]),
                    flags: i64::from(ours.flags[at]),
                    damage: ours.damage[at].iter().map(|d| i64::from(*d)).collect(),
                    spawn_base: spawn,
                    hurt_base,
                    draws: i64::from(ours.draws[at]),
                    slope: ours.slope[at],
                },
                kinds,
                hurt,
                stuck: ours.stuck[at],
            }
        })
        .collect()
}
