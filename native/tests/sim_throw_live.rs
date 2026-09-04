//! `P_SpawnMissile` and `P_CheckMissileSpawn` against a real ClickHouse
//! server.
//!
//! `demo3` throws no missile before the first divergence, so the world is
//! seeded. Every number the routine works out is compared against
//! `native/tests/support/missile.rs`, a reader written from `p_mobj.c`.
//! What the map says about the half-step is decided by where each case
//! puts the shooter: the level is swept once for a point whose half-step
//! lands and a point whose half-step is refused by a line, and the cases
//! are built on those two.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{self, map, missile, mobj};
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::missile::{Thing, Thrown, World as Oracle};
use support::mobj::thing_type;

const FRACUNIT: i64 = 1 << 16;
/// `p_mobj.h`
const MF_SOLID: i64 = 2;
const MF_SHOOTABLE: i64 = 4;
const MF_SHADOW: i64 = 0x4_0000;
/// `p_map.c`: how far a thing steps up.
const MAXSTEP: i64 = 24 * FRACUNIT;
/// `p_mobj.c`: how far above the shooter's own feet a missile starts.
const MISSILE_HEIGHT: i64 = 32 * FRACUNIT;

/// The imp's fireball, which is what `A_TroopAttack` throws.
const KIND: &str = "MT_TROOPSHOT";

/// How far east of the shooter a target stands for a shot whose angle is
/// exactly east, and the near one a steep drop needs.
const FAR: i64 = 512 * FRACUNIT;
const NEAR: i64 = 64 * FRACUNIT;

/// How far below the shooter the steep drop puts its target.
const DROP: i64 = 3000 * FRACUNIT;

/// The random indices the fan throws from, and the draw offsets it throws
/// at.
const INDICES: [i64; 4] = [0, 61, 137, 253];
const BASES: [i64; 3] = [0, 5, 17];

/// The step the sweep walks the level in, and how far each way it walks.
const STRIDE: i64 = 24 * FRACUNIT;
const REACH: i64 = 8;

#[tokio::test]
async fn a_missile_leaves_the_shooter_the_way_the_engine_throws_it() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_throw").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let start = player_start(&fixture, &db).await;
    let (open, other, walled) = sweep(&fixture, &db, start).await;
    a_walled_point_is_refused_by_a_line(&fixture, &db, walled).await;

    let (things, cases) = world(open, other, walled);
    let asks: Vec<Ask> = cases
        .iter()
        .flat_map(|case| {
            BASES
                .iter()
                .map(move |base| (case.source, case.dest, *base, case.landed))
        })
        .collect();

    for prnd in INDICES {
        let (ours, stuck) = ask_server(&fixture, &db, &things, prnd, &asks).await;
        let oracle = Oracle {
            things: things.clone(),
            prndindex: prnd,
        };
        let kind = thing_type(KIND);
        for (at, (source, dest, base, landed)) in asks.iter().enumerate() {
            let case = &cases[at / BASES.len()];
            let want = oracle.throw(*source, *dest, kind, *base, *landed);
            assert_eq!(
                ours[at], want,
                "index {prnd}, ask {at}: {} from slot {source} at slot {dest}",
                case.what
            );
            assert_eq!(
                stuck[at] == 1,
                case.stuck,
                "index {prnd}, ask {at}: {} leaves the throw stuck",
                case.what
            );
        }
        check(prnd, &ours, &cases);
    }
    fixture.finish().await;
}

/// That the fan reached every arm: a shot that lands, one a wall stops,
/// one a drop stops, and a fuzzy target that moves every draw behind it.
fn check(prnd: i64, ours: &[Thrown], cases: &[Case]) {
    let landed = ours.iter().filter(|t| !t.exploded).count();
    let gone = ours.iter().filter(|t| t.exploded).count();
    let fuzzed = ours.iter().filter(|t| t.draws == 4 || t.draws == 5).count();
    assert!(
        landed >= BASES.len() * 2 && gone >= BASES.len() * 3 && fuzzed >= BASES.len(),
        "index {prnd} reaches every arm: landed {landed}, gone {gone}, fuzzed {fuzzed}"
    );
    // The clear shot is laid out due east of its target and level with it,
    // so `R_PointToAngle2` answers zero and the missile climbs nothing.
    let clear = cases.iter().position(|c| c.what == "a clear shot").unwrap();
    let thrown = &ours[clear * BASES.len()];
    assert_eq!((thrown.angle, thrown.momz), (0, 0));
}

/// One case: what it is, whether the map lets the half-step land, and
/// whether the throw reaches the path the generator refuses.
struct Case {
    what: &'static str,
    source: i64,
    dest: i64,
    landed: bool,
    stuck: bool,
}

/// One ask: the shooter, the target, the draws before it, and what the map
/// says about the half-step.
type Ask = (i64, i64, i64, bool);

/// The seeded things and the cases thrown between them.
///
/// Every shooter stands at a point the sweep picked and every target due
/// east of it, so `R_PointToAngle2` answers exactly east and the half-step
/// is the type's own speed halved, along x alone. Only the last case's
/// blocker carries the flags `PIT_CheckThing` decides about, so every other
/// case is the geometry alone.
fn world(
    open: (i64, i64, i64),
    other: (i64, i64, i64),
    walled: (i64, i64, i64),
) -> (Vec<Thing>, Vec<Case>) {
    let mut things: Vec<Thing> = Vec::new();
    let mut cases: Vec<Case> = Vec::new();
    let mut put = |x: i64, y: i64, z: i64, flags: i64| {
        things.push(Thing { x, y, z, flags });
        things.len() as i64
    };
    let mut add = |what: &'static str,
                   at: (i64, i64, i64),
                   east: i64,
                   drop: i64,
                   shadow: bool,
                   landed: bool,
                   stuck: bool,
                   put: &mut dyn FnMut(i64, i64, i64, i64) -> i64| {
        let source = put(at.0, at.1, at.2, 0);
        let dest = put(
            at.0 + east,
            at.1,
            at.2 - drop,
            if shadow { MF_SHADOW } else { 0 },
        );
        cases.push(Case {
            what,
            source,
            dest,
            landed,
            stuck,
        });
    };
    add("a clear shot", open, FAR, 0, false, true, false, &mut put);
    add(
        "a shot into a wall",
        walled,
        FAR,
        0,
        false,
        false,
        false,
        &mut put,
    );
    add("a fuzzy target", open, FAR, 0, true, true, false, &mut put);
    add(
        "a drop the step cannot make",
        open,
        NEAR,
        DROP,
        false,
        false,
        false,
        &mut put,
    );
    add(
        "a thing in the way",
        other,
        FAR,
        0,
        false,
        false,
        true,
        &mut put,
    );
    // What the last case's half-step reaches: solid and shootable, so both
    // the move test and the guard see it.
    put(
        other.0 + half_step(),
        other.1,
        other.2,
        MF_SOLID | MF_SHOOTABLE,
    );
    (things, cases)
}

/// Where the player stands at tic 0, and the height it stands at.
async fn player_start(fixture: &Fixture, db: &str) -> (i64, i64, i64) {
    #[derive(Row, Deserialize)]
    struct Row3 {
        x: i32,
        y: i32,
        z: i32,
    }
    let row: Row3 = fixture
        .scalar(&format!(
            "SELECT m_x[p_mo] AS x, m_y[p_mo] AS y, m_z[p_mo] AS z \
             FROM {db}.native_state WHERE tic = 0"
        ))
        .await;
    (i64::from(row.x), i64::from(row.y), i64::from(row.z))
}

/// Two points whose half-step lands with room around them, far enough
/// apart that neither reaches what stands at the other, and one whose
/// half-step a line refuses.
///
/// The walk covers a square of the level around the player, asking the move
/// test about a missile-sized box at each point and five units either way
/// along each axis. A point that answers yes to all five is open; one that
/// answers yes nowhere five units east is walled.
async fn sweep(
    fixture: &Fixture,
    db: &str,
    start: (i64, i64, i64),
) -> ((i64, i64, i64), (i64, i64, i64), (i64, i64, i64)) {
    let half = half_step();
    let points: Vec<(i64, i64)> = (-REACH..=REACH)
        .flat_map(|i| (-REACH..=REACH).map(move |j| (i, j)))
        .map(|(i, j)| (start.0 + i * STRIDE, start.1 + j * STRIDE))
        .collect();
    let offsets: [(i64, i64); 5] = [(0, 0), (half, 0), (-half, 0), (0, half), (0, -half)];
    let asks: Vec<String> = points
        .iter()
        .flat_map(|(x, y)| offsets.iter().map(move |(dx, dy)| (x + dx, y + dy)))
        .map(|(x, y)| {
            box_at(
                &x.to_string(),
                &y.to_string(),
                &(start.2 + MISSILE_HEIGHT).to_string(),
            )
        })
        .collect();

    #[derive(Row, Deserialize)]
    struct Oks {
        ok: Vec<u8>,
    }
    let level = Level::of(db);
    let world = level.empty_world();
    let sql = format!(
        "WITH\n{}\nSELECT arrayMap(a -> toUInt8(a.{}), {}) AS ok",
        constants(db),
        map::answer::OK,
        map::try_moves(&format!("[{}]", asks.join(", ")), &world),
    );
    let ours: Oks = fixture.scalar(&sql).await;
    assert_eq!(ours.ok.len(), asks.len());

    let all = |at: usize| ours.ok[at * 5..at * 5 + 5].iter().all(|v| *v == 1);
    let clear: Vec<(i64, i64)> = points
        .iter()
        .enumerate()
        .filter(|(at, _)| all(*at))
        .map(|(_, p)| *p)
        .collect();
    let open = *clear
        .first()
        .expect("the level holds a point a missile can step through");
    // Far enough that what stands at one is outside the other's reach,
    // which is the two radii plus the half-step.
    let other = *clear
        .iter()
        .find(|p| (p.0 - open.0).abs() + (p.1 - open.1).abs() > 128 * FRACUNIT)
        .expect("the level holds a second such point clear of the first");
    let walled = points
        .iter()
        .enumerate()
        .find(|(at, _)| ours.ok[at * 5] == 1 && ours.ok[at * 5 + 1] == 0)
        .map(|(_, p)| *p)
        .expect("the level holds a point five units east of a wall");
    (
        (open.0, open.1, start.2),
        (other.0, other.1, start.2),
        (walled.0, walled.1, start.2),
    )
}

/// That what refuses the walled point's half-step is a line and not the
/// room's own height.
///
/// `P_TryMove`'s own tests are arithmetic on the floor and the ceiling the
/// point stands between, and `P_ThingHeightClip`'s half of
/// `P_CheckPosition` answers those two. Where all of them pass and the move
/// is still refused, a line blocked it: the sweep's world holds no things.
async fn a_walled_point_is_refused_by_a_line(fixture: &Fixture, db: &str, walled: (i64, i64, i64)) {
    #[derive(Row, Deserialize)]
    struct Heights {
        heights: Vec<(i32, i32)>,
    }
    let info = |column: &str| {
        tables::table("mobjinfo").unwrap().ints(column).unwrap()[thing_type(KIND) as usize]
    };
    let z = walled.2 + MISSILE_HEIGHT;
    let ask = box_at(
        &(walled.0 + half_step()).to_string(),
        &walled.1.to_string(),
        &z.to_string(),
    );
    let sql = format!(
        "WITH\n{}\nSELECT {} AS heights",
        constants(db),
        map::heights(&format!("[{ask}]"), &Level::of(db).empty_world()),
    );
    let ours: Heights = fixture.scalar(&sql).await;
    let (floorz, ceilingz) = (i64::from(ours.heights[0].0), i64::from(ours.heights[0].1));
    let height = info("height");
    assert!(ceilingz - floorz >= height, "the room is tall enough");
    assert!(ceilingz - z >= height, "the missile fits under the ceiling");
    assert!(floorz - z <= MAXSTEP, "the missile is not below the floor");
}

/// Half of what a missile of this type covers in a tic, which is what
/// `P_CheckMissileSpawn` moves it before the move test.
fn half_step() -> i64 {
    tables::table("mobjinfo").unwrap().ints("speed").unwrap()[thing_type(KIND) as usize] >> 1
}

/// A missile-sized move ask at a point.
fn box_at(x: &str, y: &str, z: &str) -> String {
    let info = |column: &str| {
        tables::table("mobjinfo").unwrap().ints(column).unwrap()[thing_type(KIND) as usize]
    };
    map::asking(
        "0",
        x,
        y,
        &info("radius").to_string(),
        &info("height").to_string(),
        z,
        &info("flags").to_string(),
        "0",
    )
}

/// The level's own heights, read off the row `P_SetupLevel` left.
fn at_zero(db: &str, column: &str) -> String {
    format!("joinGet('{db}.native_state', '{column}', toUInt32(0))")
}

fn constants(db: &str) -> String {
    sim::constants(db)
        .into_iter()
        .map(|(name, expr)| format!("    ({expr}) AS {name}"))
        .collect::<Vec<_>>()
        .join(",\n")
}

/// The level's own heights, as the three strings an empty world reads them
/// through.
struct Level {
    floorheight: String,
    ceilingheight: String,
    line_special: String,
}

impl Level {
    fn of(db: &str) -> Level {
        Level {
            floorheight: at_zero(db, "sec_floorheight"),
            ceilingheight: at_zero(db, "sec_ceilingheight"),
            line_special: at_zero(db, "line_special"),
        }
    }

    /// The level with nothing standing in it, so only the geometry decides.
    fn empty_world(&self) -> map::World<'_> {
        map::World {
            m_x: "CAST([], 'Array(Int32)')",
            m_y: "CAST([], 'Array(Int32)')",
            m_radius: "CAST([], 'Array(Int32)')",
            m_flags: "CAST([], 'Array(Int32)')",
            m_linkseq: "CAST([], 'Array(UInt32)')",
            alive: "CAST([], 'Array(UInt8)')",
            floorheight: &self.floorheight,
            ceilingheight: &self.ceilingheight,
            line_special: &self.line_special,
        }
    }
}

/// The answers as the statement gives them, one array per field of
/// [`missile::thrown`]. A tuple that wide has no `Deserialize`, so the
/// statement projects the list into columns.
#[derive(Row, Deserialize)]
struct Thrower {
    x: Vec<i32>,
    y: Vec<i32>,
    z: Vec<i32>,
    kind: Vec<i32>,
    state: Vec<i32>,
    tics: Vec<i32>,
    momx: Vec<i32>,
    momy: Vec<i32>,
    momz: Vec<i32>,
    angle: Vec<u32>,
    target: Vec<u32>,
    flags: Vec<i32>,
    exploded: Vec<u8>,
    draws: Vec<u32>,
    stuck: Vec<u8>,
}

fn literal(of: &[i64]) -> String {
    format!(
        "[{}]",
        of.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
    )
}

async fn ask_server(
    fixture: &Fixture,
    db: &str,
    things: &[Thing],
    prnd: i64,
    asks: &[Ask],
) -> (Vec<Thrown>, Vec<u8>) {
    let info = |column: &str| {
        tables::table("mobjinfo").unwrap().ints(column).unwrap()[thing_type(KIND) as usize]
    };
    let of = |get: &dyn Fn(&Thing) -> i64| literal(&things.iter().map(get).collect::<Vec<_>>());
    let (m_x, m_y, m_z) = (of(&|t| t.x), of(&|t| t.y), of(&|t| t.z));
    let m_flags = of(&|t| t.flags);
    // The seeded things are the size of an imp, which is what the move test
    // and the guard measure them by.
    let m_radius = of(&|_| 20 * FRACUNIT);
    let m_height = of(&|_| 56 * FRACUNIT);
    let m_linkseq = of(&|_| 1);
    let alive = format!(
        "CAST([{}], 'Array(UInt8)')",
        things.iter().map(|_| "1").collect::<Vec<_>>().join(", ")
    );
    let prndindex = prnd.to_string();
    let throwing = missile::Throwing {
        m_x: &m_x,
        m_y: &m_y,
        m_z: &m_z,
        m_radius: &m_radius,
        m_height: &m_height,
        m_flags: &m_flags,
        prndindex: &prndindex,
    };
    let (floorheight, ceilingheight) = (
        at_zero(db, "sec_floorheight"),
        at_zero(db, "sec_ceilingheight"),
    );
    let spawning = mobj::Spawning {
        floorheight: &floorheight,
        ceilingheight: &ceilingheight,
        prndindex: &prndindex,
        skill: "2",
    };
    let line_special = at_zero(db, "line_special");
    let world = map::World {
        m_x: &m_x,
        m_y: &m_y,
        m_radius: &m_radius,
        m_flags: &m_flags,
        m_linkseq: &m_linkseq,
        alive: &alive,
        floorheight: &floorheight,
        ceilingheight: &ceilingheight,
        line_special: &line_special,
    };
    let kind = thing_type(KIND);
    let list = format!(
        "[{}]",
        asks.iter()
            .map(|(source, dest, base, _)| format!(
                "(toUInt32({source}), toUInt32({dest}), toInt32({kind}), toUInt32({base}))"
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let column = |name: &str, field: usize| format!("arrayMap(t -> t.{field}, ms) AS {name}");
    let columns = [
        column("x", missile::thrown::X),
        column("y", missile::thrown::Y),
        column("z", missile::thrown::Z),
        column("kind", missile::thrown::TYPE),
        column("state", missile::thrown::STATE),
        column("tics", missile::thrown::TICS),
        column("momx", missile::thrown::MOMX),
        column("momy", missile::thrown::MOMY),
        column("momz", missile::thrown::MOMZ),
        column("angle", missile::thrown::ANGLE),
        column("target", missile::thrown::TARGET),
        column("flags", missile::thrown::FLAGS),
        column("exploded", missile::thrown::EXPLODED),
        column("draws", missile::thrown::DRAWS),
        column("stuck", missile::thrown::STUCK),
    ];
    let sql = format!(
        "SELECT\n    {}\nFROM\n(\n    WITH\n{},\n    ({list}) AS ms_asks\n    \
         SELECT {} AS ms\n)",
        columns.join(",\n    "),
        constants(db),
        missile::spawn("ms_asks", &throwing, &spawning, &world),
    );
    let ours: Thrower = fixture.scalar(&sql).await;
    let _ = info("radius");
    let thrown = (0..ours.x.len())
        .map(|at| Thrown {
            x: i64::from(ours.x[at]),
            y: i64::from(ours.y[at]),
            z: i64::from(ours.z[at]),
            kind: i64::from(ours.kind[at]),
            state: i64::from(ours.state[at]),
            tics: i64::from(ours.tics[at]),
            momx: i64::from(ours.momx[at]),
            momy: i64::from(ours.momy[at]),
            momz: i64::from(ours.momz[at]),
            angle: i64::from(ours.angle[at]),
            target: i64::from(ours.target[at]),
            flags: i64::from(ours.flags[at]),
            exploded: ours.exploded[at] == 1,
            draws: i64::from(ours.draws[at]),
        })
        .collect();
    (thrown, ours.stuck)
}
