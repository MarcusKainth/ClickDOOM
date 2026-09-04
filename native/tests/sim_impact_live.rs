//! What a missile in flight does, against a real ClickHouse server.
//!
//! Three things, each seeded because `demo3` reaches none of them before
//! the first divergence. `PIT_CheckThing`'s missile branch, over lists of
//! things the move test's box reached. `P_ExplodeMissile` with
//! `P_XYMovement`'s sky check ahead of it, over the level's own lines. And
//! the death frames a missile that went off runs out, which is the state
//! cycle `P_MobjThinker` already runs.
//!
//! The first two are compared against `native/tests/support/missile.rs`, a
//! reader written from `p_map.c` and `p_mobj.c`.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{self, map, missile};
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::missile::{Missile, Reached, Stopped, World as Oracle};
use support::mobj::thing_type;
use support::seed;

const FRACUNIT: i64 = 1 << 16;
/// `p_mobj.h`
const MF_SOLID: i64 = 2;
const MF_SHOOTABLE: i64 = 4;
const MF_MISSILE: i64 = 0x1_0000;

/// The imp's fireball, which is what `A_TroopAttack` throws.
const KIND: &str = "MT_TROOPSHOT";

/// The random indices the fan runs from, and the draw offsets it runs at.
const INDICES: [i64; 4] = [0, 61, 137, 253];
const BASES: [i64; 3] = [0, 5, 17];

/// `mobjinfo`'s own number for a type, by name.
fn info(kind: &str, column: &str) -> i64 {
    tables::table("mobjinfo").unwrap().ints(column).unwrap()[thing_type(kind) as usize]
}

fn state_tics(state: i64) -> i64 {
    tables::table("states").unwrap().ints("tics").unwrap()[state as usize]
}

fn next_state(state: i64) -> i64 {
    tables::table("states").unwrap().ints("nextstate").unwrap()[state as usize]
}

/// The first type the engine ships that is solid and cannot be shot, which
/// is what stops a missile without taking damage.
fn solid_but_not_shootable() -> i64 {
    let flags = tables::table("mobjinfo").unwrap().ints("flags").unwrap();
    flags
        .iter()
        .position(|held| held & MF_SOLID != 0 && held & MF_SHOOTABLE == 0)
        .expect("the engine ships a solid thing that cannot be shot") as i64
}

/// One slot of the seeded world.
#[derive(Clone)]
struct Entry {
    z: i64,
    height: i64,
    kind: i64,
    flags: i64,
    state: i64,
    tics: i64,
    /// The slot this one points at, 0 for none.
    target: usize,
}

impl Entry {
    fn reached(&self) -> Reached {
        Reached {
            z: self.z,
            height: self.height,
            kind: self.kind,
            flags: self.flags,
        }
    }
}

// ---------------------------------------------------------------------------
// PIT_CheckThing's missile branch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_impact_decides_the_way_the_engine_decides() {
    let fixture = load_level("sim_impact").await;
    let db = fixture.database.clone();

    let (world, shots, lists) = seeded_walk();
    let asks: Vec<(usize, Vec<usize>)> = shots
        .iter()
        .flat_map(|slot| lists.iter().map(move |list| (*slot, list.clone())))
        .collect();

    for prnd in INDICES {
        for base in BASES {
            let ours = ask_impact(&fixture, &db, &world, prnd, &asks, base).await;
            let oracle = Oracle {
                things: Vec::new(),
                prndindex: prnd,
            };
            for (at, (slot, list)) in asks.iter().enumerate() {
                let it = &world[slot - 1];
                let shooter = if it.target == 0 {
                    -1
                } else {
                    world[it.target - 1].kind
                };
                let missile = Missile {
                    z: it.z,
                    height: it.height,
                    kind: it.kind,
                    shooter,
                };
                let touched: Vec<Reached> = list.iter().map(|k| world[*k - 1].reached()).collect();
                let shooter_at = list
                    .iter()
                    .position(|k| *k == it.target)
                    .map_or(0, |at| at + 1);
                let want = oracle.strike(&missile, &touched, shooter_at, base);
                let hit = if want.at == 0 {
                    0
                } else {
                    list[want.at - 1] as i64
                };
                assert_eq!(
                    ours[at],
                    (hit, want.blocked, want.damage, want.draws),
                    "index {prnd}, base {base}, ask {at}: missile {slot} over {list:?}"
                );
            }
            check_walk(prnd, &ours);
        }
    }
    fixture.finish().await;
}

/// That the fan reached every arm of the walk.
fn check_walk(prnd: i64, ours: &[(i64, bool, i64, i64)]) {
    let walked = ours.iter().filter(|a| !a.1).count();
    let stopped = ours.iter().filter(|a| a.1 && a.3 == 0).count();
    let damaged = ours.iter().filter(|a| a.1 && a.3 == 1).count();
    assert!(
        walked > 0 && stopped > 0 && damaged > 0,
        "index {prnd} reaches every arm: walked {walked}, stopped {stopped}, damaged {damaged}"
    );
    assert!(
        ours.iter().any(|a| a.1 && a.2 > 0),
        "index {prnd} damages what it stops on"
    );
}

/// The seeded world, the slots the missiles sit in, and the lists of slots
/// a move test's box reached.
///
/// Every arm of the walk is in the lists: a thing the missile passes over,
/// one it passes under, one of the shooter's own species, a player of it,
/// whatever fired it, one that cannot be shot and is not solid, one that
/// cannot be shot and is, and one that can.
fn seeded_walk() -> (Vec<Entry>, Vec<usize>, Vec<Vec<usize>>) {
    let z = 100 * FRACUNIT;
    let mut world: Vec<Entry> = Vec::new();
    let mut put = |z: i64, height: i64, kind: i64, flags: i64, target: usize| {
        world.push(Entry {
            z,
            height,
            kind,
            flags,
            state: info("MT_TROOP", "spawnstate"),
            tics: 7,
            target,
        });
        world.len()
    };
    let troop = thing_type("MT_TROOP");
    let player = thing_type("MT_PLAYER");
    let solid = MF_SOLID | MF_SHOOTABLE;
    let above = put(z + 200 * FRACUNIT, 56 * FRACUNIT, troop, solid, 0);
    let below = put(z - 200 * FRACUNIT, 8 * FRACUNIT, troop, solid, 0);
    let same = put(z, 56 * FRACUNIT, troop, solid, 0);
    let man = put(z, 56 * FRACUNIT, player, solid, 0);
    let ghost = put(z, 16 * FRACUNIT, thing_type("MT_PUFF"), 0, 0);
    let post = put(z, 56 * FRACUNIT, solid_but_not_shootable(), MF_SOLID, 0);
    let target = put(z, 56 * FRACUNIT, thing_type("MT_SERGEANT"), solid, 0);
    let baron = put(z, 64 * FRACUNIT, thing_type("MT_BRUISER"), solid, 0);

    // The missiles: one fired by an imp, one by a player, one by a baron,
    // and one by nothing at all. Each points at a thing of its shooter's
    // type, which is what the species test reads.
    let kind = thing_type(KIND);
    let height = info(KIND, "height");
    let shots = vec![
        put(z, height, kind, info(KIND, "flags"), same),
        put(z, height, kind, info(KIND, "flags"), man),
        put(z, height, kind, info(KIND, "flags"), baron),
        put(z, height, kind, info(KIND, "flags"), 0),
    ];
    let lists = vec![
        vec![],
        vec![above, below],
        vec![above, target],
        vec![same],
        vec![man],
        vec![ghost, post],
        vec![ghost, target],
        vec![post, target],
        vec![above, below, ghost, same, target],
        // The thing that fired it, which the walk hands back untouched
        // whatever else it would have decided.
        vec![same, target],
        vec![man, target],
    ];
    (world, shots, lists)
}

/// The impact over every ask, as `(hit, blocked, damage, draws)`.
async fn ask_impact(
    fixture: &Fixture,
    db: &str,
    world: &[Entry],
    prnd: i64,
    asks: &[(usize, Vec<usize>)],
    base: i64,
) -> Vec<(i64, bool, i64, i64)> {
    let arrays = Arrays::of(world);
    let prndindex = prnd.to_string();
    let flying = arrays.flying(&prndindex);
    let list = format!(
        "[{}]",
        asks.iter()
            .map(|(slot, touched)| format!(
                "(toUInt32({slot}), CAST([{}], 'Array(UInt32)'), toUInt32({base}))",
                touched
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    #[derive(Row, Deserialize)]
    struct Struck {
        hit: Vec<u32>,
        blocked: Vec<u8>,
        damage: Vec<i32>,
        draws: Vec<u32>,
    }
    let column = |name: &str, field: usize| format!("arrayMap(t -> t.{field}, st) AS {name}");
    let sql = format!(
        "SELECT\n    {}\nFROM\n(\n    WITH\n{},\n    ({list}) AS st_asks\n    \
         SELECT {} AS st\n)",
        [
            column("hit", missile::struck::HIT),
            column("blocked", missile::struck::BLOCKED),
            column("damage", missile::struck::DAMAGE),
            column("draws", missile::struck::DRAWS),
        ]
        .join(",\n    "),
        constants(db),
        missile::impact("st_asks", &flying),
    );
    let ours: Struck = fixture.scalar(&sql).await;
    (0..ours.hit.len())
        .map(|at| {
            (
                i64::from(ours.hit[at]),
                ours.blocked[at] == 1,
                i64::from(ours.damage[at]),
                i64::from(ours.draws[at]),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// P_ExplodeMissile and the sky check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stopped_missile_goes_off_or_the_sky_takes_it() {
    let fixture = load_level("sim_stop").await;
    let db = fixture.database.clone();

    // Three of the level's own lines: one with no sky either side, one
    // whose front sector is the sky and whose back is not, and one whose
    // back sector is the sky. Only the last takes the missile off, and the
    // middle one is what tells the back sector from the front.
    let solid = sky_line(&fixture, &db, false, Some(false)).await;
    let front = sky_line(&fixture, &db, false, Some(true)).await;
    let sky = sky_line(&fixture, &db, true, None).await;
    let kind = thing_type(KIND);
    let (state, tics, flags) = (info(KIND, "spawnstate"), 7, info(KIND, "flags"));
    let world = vec![Entry {
        z: 100 * FRACUNIT,
        height: info(KIND, "height"),
        kind,
        flags,
        state,
        tics,
        target: 0,
    }];
    let lines = [-1, solid, front, sky];

    for prnd in INDICES {
        for base in BASES {
            let ours = ask_explode(&fixture, &db, &world, &lines, prnd, base).await;
            let oracle = Oracle {
                things: Vec::new(),
                prndindex: prnd,
            };
            for (at, line) in lines.iter().enumerate() {
                let want = oracle.stop(kind, state, tics, flags, *line == sky, base);
                assert_eq!(ours[at], want, "index {prnd}, base {base}, line {line}");
            }
            assert!(ours[3].removed, "a sky ceiling behind it takes it off");
            assert!(
                !ours[0].removed && !ours[1].removed && !ours[2].removed,
                "a sky ceiling in front of it does not"
            );
            assert_eq!(ours[0].flags & MF_MISSILE, 0, "it stops being a missile");
        }
    }
    fixture.finish().await;
}

/// The level's own first two-sided line with the sky where it is asked
/// for. `front` is left out where the case does not care.
///
/// `E1M7` carries no line whose back sector is the sky and whose front is
/// not, so the case that tells the two sectors apart is the one with the
/// sky in front only.
async fn sky_line(fixture: &Fixture, db: &str, back: bool, front: Option<bool>) -> i64 {
    #[derive(Row, Deserialize)]
    struct One {
        id: i32,
    }
    let sky = format!(
        "(SELECT id FROM {db}.lv_sectors_static WHERE ceilingpic = \
         (SELECT toInt32(id) FROM {db}.flats WHERE upper(name) = 'F_SKY1'))"
    );
    let is =
        |column: &str, want: bool| format!("{column} {} IN {sky}", if want { "" } else { "NOT" });
    let row: One = fixture
        .scalar(&format!(
            "SELECT toInt32(id) AS id FROM {db}.lv_lines WHERE side1 != -1 \
             AND {} AND {} ORDER BY id LIMIT 1",
            is("sector1", back),
            front.map_or_else(|| "1".to_owned(), |want| is("sector0", want)),
        ))
        .await;
    i64::from(row.id)
}

async fn ask_explode(
    fixture: &Fixture,
    db: &str,
    world: &[Entry],
    lines: &[i64],
    prnd: i64,
    base: i64,
) -> Vec<Stopped> {
    let arrays = Arrays::of(world);
    let prndindex = prnd.to_string();
    let flying = arrays.flying(&prndindex);
    let list = format!(
        "[{}]",
        lines
            .iter()
            .map(|line| format!("(toUInt32(1), toInt32({line}), toUInt32({base}))"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    #[derive(Row, Deserialize)]
    struct Ended {
        state: Vec<i32>,
        tics: Vec<i32>,
        flags: Vec<i32>,
        removed: Vec<u8>,
        draws: Vec<u32>,
        stuck: Vec<u8>,
    }
    let column = |name: &str, field: usize| format!("arrayMap(t -> t.{field}, ex) AS {name}");
    let sql = format!(
        "SELECT\n    {}\nFROM\n(\n    WITH\n{},\n    ({list}) AS ex_asks\n    \
         SELECT {} AS ex\n)",
        [
            column("state", missile::stopped::STATE),
            column("tics", missile::stopped::TICS),
            column("flags", missile::stopped::FLAGS),
            column("removed", missile::stopped::REMOVED),
            column("draws", missile::stopped::DRAWS),
            column("stuck", missile::stopped::STUCK),
        ]
        .join(",\n    "),
        constants(db),
        missile::explode("ex_asks", &flying),
    );
    let ours: Ended = fixture.scalar(&sql).await;
    (0..ours.state.len())
        .map(|at| {
            assert_eq!(ours.stuck[at], 0, "ask {at} reached no unwritten path");
            Stopped {
                state: i64::from(ours.state[at]),
                tics: i64::from(ours.tics[at]),
                flags: i64::from(ours.flags[at]),
                removed: ours.removed[at] == 1,
                draws: i64::from(ours.draws[at]),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// What the move test hands the walk
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_move_test_hands_back_every_thing_the_box_reaches() {
    let fixture = load_level("sim_touched").await;
    let db = fixture.database.clone();

    #[derive(Row, Deserialize)]
    struct Reach {
        x: i32,
        y: i32,
    }
    let start: Reach = fixture
        .scalar(&format!(
            "SELECT m_x[p_mo] AS x, m_y[p_mo] AS y FROM {db}.native_state WHERE tic = 0"
        ))
        .await;
    let (x, y) = (i64::from(start.x), i64::from(start.y));

    // Two solid things the box reaches. Both sit in one blockmap cell, so
    // the walk reaches the one linked last first, and that one blocks. A
    // missile passes over or under it and decides about the other, so a
    // list cut at the first that blocks would lose the second.
    let of = |values: [i64; 2]| format!("[{}, {}]", values[0], values[1]);
    let radius = 20 * FRACUNIT;
    let world = map::World {
        m_x: &of([x, x + 8 * FRACUNIT]),
        m_y: &of([y, y]),
        m_radius: &of([radius, radius]),
        m_flags: &of([MF_SOLID | MF_SHOOTABLE, MF_SOLID | MF_SHOOTABLE]),
        m_linkseq: "CAST([1, 2], 'Array(UInt32)')",
        alive: "CAST([1, 1], 'Array(UInt8)')",
        floorheight: &at_zero(&db, "sec_floorheight"),
        ceilingheight: &at_zero(&db, "sec_ceilingheight"),
        line_special: &at_zero(&db, "line_special"),
    };
    let ask = map::asking(
        "0",
        &x.to_string(),
        &y.to_string(),
        &radius.to_string(),
        &(56 * FRACUNIT).to_string(),
        "0",
        &info(KIND, "flags").to_string(),
        "0",
    );
    #[derive(Row, Deserialize)]
    struct Touched {
        ok: u8,
        touched: Vec<u32>,
    }
    let ours: Touched = fixture
        .scalar(&format!(
            "SELECT toUInt8(a.{}) AS ok, arrayMap(k -> toUInt32(k), a.{}) AS touched \
             FROM (WITH\n{}\nSELECT ({})[1] AS a)",
            map::answer::OK,
            map::answer::TOUCHED,
            constants(&db),
            map::try_moves(&format!("[{ask}]"), &world),
        ))
        .await;
    fixture.finish().await;

    assert_eq!(ours.ok, 0, "a solid thing in the box refuses the move");
    assert_eq!(
        ours.touched,
        vec![2, 1],
        "the list is not cut at the first thing that blocks"
    );
}

/// The level's own heights, read off the row `P_SetupLevel` left.
fn at_zero(db: &str, column: &str) -> String {
    format!("joinGet(\'{db}.native_state\', \'{column}\', toUInt32(0))")
}

// ---------------------------------------------------------------------------
// The death frames
// ---------------------------------------------------------------------------

/// The tic the seeded row copies, and the row's own tic.
const BEFORE: u32 = 40;
const SEED_TIC: u32 = 41;
/// The slot the seeded missile is put in. It is the last on the list, so
/// nothing points at it when it goes.
const SLOT: usize = 250;

#[tokio::test]
async fn a_missile_that_went_off_runs_out_its_death_frames() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_death").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    // The chain `P_ExplodeMissile` leaves the thing at the top of, and how
    // long it takes to run out.
    let mut chain = vec![info(KIND, "deathstate")];
    while next_state(*chain.last().expect("a death frame")) != 0 {
        chain.push(next_state(*chain.last().expect("a death frame")));
    }
    let waits: i64 = chain.iter().map(|state| state_tics(*state)).sum();
    assert!(chain.len() > 1 && waits > 2, "{chain:?} waits {waits}");

    let kind = thing_type(KIND);
    let put = |column: &'static str, cast: &str, value: String| {
        (
            column,
            format!(
                "arrayMap((v, k) -> {cast}(if(k = {SLOT}, {value}, v)), \
                 p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    };
    // What `P_ExplodeMissile` leaves: no momentum, the death frame with
    // its own wait, and the thing no longer a missile. It stands on its
    // floor, so `P_ZMovement` does nothing to it either.
    let overrides = [
        put("m_type", "toInt32", kind.to_string()),
        put("m_state", "toInt32", chain[0].to_string()),
        put("m_tics", "toInt32", state_tics(chain[0]).to_string()),
        put(
            "m_flags",
            "toInt32",
            (info(KIND, "flags") & !MF_MISSILE).to_string(),
        ),
        put("m_height", "toInt32", info(KIND, "height").to_string()),
        put("m_radius", "toInt32", info(KIND, "radius").to_string()),
        put("m_momx", "toInt32", "0".to_owned()),
        put("m_momy", "toInt32", "0".to_owned()),
        put("m_momz", "toInt32", "0".to_owned()),
        put("m_z", "toInt32", format!("p.m_floorz[{SLOT}]")),
        put("m_target", "toUInt32", "0".to_owned()),
    ];
    let seeded: Vec<sql::Statement> = seed::row(&db, SEED_TIC, BEFORE, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let last = SEED_TIC + 1 + waits as u32;
    let run = sim::tick::demo_statement(&db, SEED_TIC + 1, last);
    if let Err(error) = fixture.execute(&[run]).await {
        fixture.finish().await;
        panic!("{error}");
    }

    // The seeded thing is followed by its type, not by the slot it stands
    // in: a removal anywhere below it moves it down, and `m_id` holds the
    // slot rather than an identity of its own. `E1M7` puts no other
    // fireball on the list.
    #[derive(Row, Deserialize)]
    struct Cycle {
        tic: u32,
        at: u64,
        state: i32,
        unresolved: u8,
    }
    let rows: Vec<Cycle> = fixture
        .rows(&format!(
            "SELECT tic, toUInt64(indexOf(m_type, toInt32({kind}))) AS at, \
             toInt32(if(indexOf(m_type, toInt32({kind})) = 0, -1, \
             m_state[indexOf(m_type, toInt32({kind}))])) AS state, \
             unresolved FROM {db}.native_state \
             WHERE tic BETWEEN {SEED_TIC} AND {last} ORDER BY tic"
        ))
        .await;
    fixture.finish().await;

    assert!(
        rows.iter().all(|row| row.unresolved == 0),
        "every tic of the cycle was produced: {:?}",
        rows.iter()
            .map(|row| (row.tic, row.unresolved))
            .collect::<Vec<_>>()
    );
    let seen: Vec<i32> = rows.iter().map(|row| row.state).collect();
    for state in &chain {
        assert!(
            seen.contains(&(*state as i32)),
            "the cycle enters every death frame: {:?} against {chain:?}",
            rows.iter()
                .map(|row| (row.tic, row.at, row.state))
                .collect::<Vec<_>>()
        );
    }
    let ended = rows.last().expect("the run made rows");
    assert_eq!(
        ended.at, 0,
        "the thing leaves the list when its cycle ends: {seen:?}"
    );
    assert_eq!(
        rows.iter().position(|row| row.at == 0),
        Some(waits as usize),
        "it stays on the list for as long as its frames wait: {seen:?}"
    );
}

// ---------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------

/// The level loaded and nothing run, which is what the two fans need.
async fn load_level(case: &str) -> Fixture {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create(case).await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    fixture
}

/// The seeded world as the array literals a primitive reads it through.
struct Arrays {
    m_z: String,
    m_height: String,
    m_type: String,
    m_state: String,
    m_tics: String,
    m_flags: String,
    m_target: String,
}

impl Arrays {
    fn of(world: &[Entry]) -> Arrays {
        let of = |get: &dyn Fn(&Entry) -> i64| {
            format!(
                "[{}]",
                world
                    .iter()
                    .map(|e| get(e).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        Arrays {
            m_z: of(&|e| e.z),
            m_height: of(&|e| e.height),
            m_type: of(&|e| e.kind),
            m_state: of(&|e| e.state),
            m_tics: of(&|e| e.tics),
            m_flags: of(&|e| e.flags),
            m_target: of(&|e| e.target as i64),
        }
    }

    fn flying<'a>(&'a self, prndindex: &'a str) -> missile::Flying<'a> {
        missile::Flying {
            m_z: &self.m_z,
            m_height: &self.m_height,
            m_type: &self.m_type,
            m_state: &self.m_state,
            m_tics: &self.m_tics,
            m_flags: &self.m_flags,
            m_target: &self.m_target,
            prndindex,
        }
    }
}

fn constants(db: &str) -> String {
    sim::constants(db)
        .into_iter()
        .map(|(name, expr)| format!("    ({expr}) AS {name}"))
        .collect::<Vec<_>>()
        .join(",\n")
}
