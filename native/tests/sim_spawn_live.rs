//! `P_SpawnMobj`, `P_SpawnPuff` and `P_SpawnBlood` against a real
//! ClickHouse server.
//!
//! A spawn is where the shot's puffs and blood come from and where a
//! killed thing's drop comes from, so it is checked on its own against
//! `native/tests/support/mobj.rs`, a reader written from `p_mobj.c`. The
//! fan spawns across `E1M7` at every damage and range the engine branches
//! on, from a spread of random indices.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{self, mobj};
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::mobj::{Born, Debris, World, thing_type};

const FRACUNIT: i64 = 1 << 16;
/// `p_local.h`
const MELEERANGE: i64 = 64 * FRACUNIT;
const MISSILERANGE: i64 = 32 * 64 * FRACUNIT;
/// `p_mobj.h`
const ONFLOORZ: i64 = i32::MIN as i64;
const ONCEILINGZ: i64 = i32::MAX as i64;

/// The random indices the fan spawns from. A spawn's own four draws land
/// at different places in the table for each.
const INDICES: [i64; 4] = [0, 61, 175, 253];
/// What a gunshot can do, which is what picks a blood spot's frame: the
/// three `5 * (P_Random() % 3 + 1)` gives, and one under nine.
const DAMAGE: [i64; 4] = [5, 10, 15, 3];

#[tokio::test]
async fn a_spawn_leaves_what_the_engine_leaves() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_spawn").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let (world, points, skill) = read_world(&fixture, &wad).await;
    assert!(points.len() > 8, "the fan spawns in several sectors");

    let debris = debris(&points);
    assert!(debris.len() > 100, "the fan is worth running");
    for prnd in INDICES {
        let ours = ask_debris(&fixture, &db, &world, skill, prnd, &debris).await;
        let want: Vec<Born> = debris.iter().map(|ask| world.debris(prnd, ask)).collect();
        check(&format!("the debris from index {prnd}"), &ours, &want);
    }
    the_fan_reaches_every_frame(&world, &debris);

    let plain = plain(&points);
    let ours = ask_spawn(&fixture, &db, &world, skill, INDICES[1], &plain).await;
    let want: Vec<Born> = plain
        .iter()
        .map(|(kind, x, y, z, base)| world.spawn(INDICES[1], *kind, *x, *y, *z, *base))
        .collect();
    check("the spawn", &ours, &want);
    let on_floor = want.iter().filter(|born| born.z == born.floorz).count();
    let under_ceiling = want.iter().filter(|born| born.z < born.ceilingz).count();
    assert!(
        on_floor > 4 && under_ceiling > 4,
        "the fan asks for the floor and the ceiling: {on_floor} {under_ceiling}"
    );

    fixture.finish().await;
}

fn check(what: &str, ours: &[Born], want: &[Born]) {
    assert_eq!(ours.len(), want.len());
    for (at, born) in want.iter().enumerate() {
        assert_eq!(&ours[at], born, "{what}, spawn {at}");
    }
}

/// That the fan reached each frame `P_SetMobjState` moves a spawn to, and
/// the shortened wait a spawn that stays where it is keeps.
fn the_fan_reaches_every_frame(world: &World, asks: &[Debris]) {
    let mut frames: Vec<i64> = Vec::new();
    let mut shortened = 0;
    for prnd in INDICES {
        for ask in asks {
            let born = world.debris(prnd, ask);
            if !frames.contains(&born.state) {
                frames.push(born.state);
            }
            let spawn = world.spawn(prnd, born.kind, ask.x, ask.y, ask.z, ask.base);
            if born.state == spawn.state && born.tics != spawn.tics {
                shortened += 1;
            }
        }
    }
    assert!(
        frames.len() >= 5,
        "the fan reaches every frame a spawn is moved to: {frames:?}"
    );
    assert!(
        shortened > 20,
        "a spawn that stays keeps a shortened wait: {shortened}"
    );
}

/// The debris asks: every point at both kinds, every damage and both
/// ranges, each at a base of its own so no two share their draws.
fn debris(points: &[(i64, i64, i64)]) -> Vec<Debris> {
    let mut asks = Vec::new();
    for (x, y, z) in points {
        for blood in [false, true] {
            for damage in DAMAGE {
                for range in [MELEERANGE, MISSILERANGE] {
                    asks.push(Debris {
                        blood,
                        x: *x,
                        y: *y,
                        z: *z,
                        damage,
                        range,
                        base: asks.len() as i64 % 17,
                    });
                }
            }
        }
    }
    asks
}

/// The plain spawns: one of each of a few types at every point, asked for
/// the floor, the ceiling and a height of its own.
fn plain(points: &[(i64, i64, i64)]) -> Vec<(i64, i64, i64, i64, i64)> {
    let kinds = ["MT_POSSESSED", "MT_BARREL", "MT_CLIP", "MT_TROOPSHOT"].map(thing_type);
    let mut asks = Vec::new();
    for (x, y, z) in points {
        for kind in kinds {
            for asked in [ONFLOORZ, ONCEILINGZ, *z] {
                asks.push((kind, *x, *y, asked, asks.len() as i64 % 13));
            }
        }
    }
    asks
}

/// One spawned thing as the statement gives it, in [`mobj::born`]'s
/// order.
type BornRow = (
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    u32,
);

#[derive(Row, Deserialize)]
struct Spawned {
    born: Vec<BornRow>,
}

fn read(spawned: Spawned) -> Vec<Born> {
    spawned
        .born
        .into_iter()
        .map(|b| Born {
            x: i64::from(b.0),
            y: i64::from(b.1),
            z: i64::from(b.2),
            kind: i64::from(b.3),
            state: i64::from(b.4),
            tics: i64::from(b.5),
            floorz: i64::from(b.6),
            ceilingz: i64::from(b.7),
            subsector: i64::from(b.8),
            lastlook: i64::from(b.9),
            reactiontime: i64::from(b.10),
            momz: i64::from(b.11),
            draws: i64::from(b.12),
        })
        .collect()
}

fn literal(of: &[i64]) -> String {
    format!(
        "[{}]",
        of.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
    )
}

fn spawning<'a>(
    floorheight: &'a str,
    ceilingheight: &'a str,
    prnd: &'a str,
    skill: &'a str,
) -> mobj::Spawning<'a> {
    mobj::Spawning {
        floorheight,
        ceilingheight,
        prndindex: prnd,
        skill,
    }
}

async fn issue(fixture: &Fixture, db: &str, list: String, answer: String) -> Vec<Born> {
    let sql = format!(
        "WITH\n{},\n    ({list}) AS sp_asks\nSELECT {answer} AS born",
        sim::constants(db)
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
    );
    read(fixture.scalar(&sql).await)
}

async fn ask_debris(
    fixture: &Fixture,
    db: &str,
    world: &World,
    skill: i64,
    prnd: i64,
    asks: &[Debris],
) -> Vec<Born> {
    let (floor, ceiling) = (literal(&world.floorheight), literal(&world.ceilingheight));
    let (prnd, skill) = (prnd.to_string(), skill.to_string());
    let asking = spawning(&floor, &ceiling, &prnd, &skill);
    let list = format!(
        "[{}]",
        asks.iter()
            .map(|a| format!(
                "(toUInt8({}), toInt32({}), toInt32({}), toInt32({}), \
                 toInt32({}), toInt32({}), toUInt32({}))",
                u8::from(a.blood),
                a.x,
                a.y,
                a.z,
                a.damage,
                a.range,
                a.base
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    issue(fixture, db, list, mobj::spawn_debris("sp_asks", &asking)).await
}

async fn ask_spawn(
    fixture: &Fixture,
    db: &str,
    world: &World,
    skill: i64,
    prnd: i64,
    asks: &[(i64, i64, i64, i64, i64)],
) -> Vec<Born> {
    let (floor, ceiling) = (literal(&world.floorheight), literal(&world.ceilingheight));
    let (prnd, skill) = (prnd.to_string(), skill.to_string());
    let asking = spawning(&floor, &ceiling, &prnd, &skill);
    let list = format!(
        "[{}]",
        asks.iter()
            .map(|(kind, x, y, z, base)| format!(
                "(toInt32({kind}), toInt32({x}), toInt32({y}), toInt32({z}), toUInt32({base}))"
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    issue(fixture, db, list, mobj::spawn_mobj("sp_asks", &asking)).await
}

#[derive(Row, Deserialize)]
struct Sector {
    floorheight: i32,
    ceilingheight: i32,
}

#[derive(Row, Deserialize)]
struct Subsector {
    sector: u32,
}

#[derive(Row, Deserialize)]
struct Spot {
    x: i32,
    y: i32,
    z: i32,
}

#[derive(Row, Deserialize)]
struct Skill {
    skill: i32,
}

/// The level, the points the fan spawns at, and the skill the demo runs.
async fn read_world(fixture: &Fixture, wad: &Wad<'_>) -> (World, Vec<(i64, i64, i64)>, i64) {
    let db = &fixture.database;
    let sectors: Vec<Sector> = fixture
        .rows(&format!(
            "SELECT floorheight, ceilingheight FROM {db}.lv_sectors_static ORDER BY id"
        ))
        .await;
    let subsectors: Vec<Subsector> = fixture
        .rows(&format!(
            "SELECT sector FROM {db}.lv_subsectors ORDER BY id"
        ))
        .await;
    let skill: Skill = fixture
        .scalar(&format!(
            "SELECT toInt32(skill) AS skill FROM {db}.demo_header LIMIT 1"
        ))
        .await;
    // The mobjs the level starts with, which stand in sectors of their own.
    let spots: Vec<Spot> = fixture
        .scalar::<Spots>(&format!(
            "SELECT arrayMap((x, y, z) -> (x, y, z), m_x, m_y, m_z) AS spots \
             FROM {db}.native_state WHERE tic = 0"
        ))
        .await
        .spots
        .into_iter()
        .map(|(x, y, z)| Spot { x, y, z })
        .collect();
    let points: Vec<(i64, i64, i64)> = spots
        .iter()
        .step_by(spots.len() / 12)
        .map(|s| {
            (
                i64::from(s.x),
                i64::from(s.y),
                i64::from(s.z) + 24 * FRACUNIT,
            )
        })
        .collect();
    let world = World {
        nodes: support::spawn::nodes(wad, support::MAP),
        ssec_sector: subsectors.iter().map(|s| s.sector as usize).collect(),
        floorheight: sectors.iter().map(|s| i64::from(s.floorheight)).collect(),
        ceilingheight: sectors.iter().map(|s| i64::from(s.ceilingheight)).collect(),
        skill: i64::from(skill.skill),
    };
    (world, points, i64::from(skill.skill))
}

#[derive(Row, Deserialize)]
struct Spots {
    spots: Vec<(i32, i32, i32)>,
}
