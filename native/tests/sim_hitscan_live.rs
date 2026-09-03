//! `P_LineAttack` against a real ClickHouse server.
//!
//! `PTR_ShootTraverse` decides where a shot ends and what goes there, so it
//! is checked on its own against `native/tests/support/shoot.rs`, a reader
//! written from `p_map.c`. The fan is the mobjs `P_SetupLevel` leaves on
//! `E1M7` shooting at each other over the level's own geometry, at the
//! slope `P_BulletSlope` gives and at slopes either side of it.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{self, maputl, shoot};
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::shoot::{Ask, Level, Shot, read_level};
use support::traverse::Thing;

const FRACUNIT: i64 = 1 << 16;
/// `p_local.h`: how far a hitscan reaches.
const MISSILERANGE: i64 = 32 * 64 * FRACUNIT;

/// How many of the level's mobjs shoot, spread evenly through the list,
/// and how many of the ones nearest each it shoots at.
const SHOOTERS: usize = 16;
const NEAREST: usize = 8;
/// How many angles of a full turn each shooter also sweeps, which is what
/// reaches the walls and the empty air.
const SWEEP: u32 = 16;
/// The slopes each shot is asked at, on top of the one `P_BulletSlope`
/// gives: straight ahead, and steeply up and down, which is what sends a
/// shot into a ceiling or a floor rather than through the opening.
const SLOPES: [i64; 3] = [0, 30000, -30000];
/// How many asks one statement carries. The list travels as a literal, so
/// a whole fan in one statement is past the server's `max_query_size`.
const BATCH: usize = 256;

#[tokio::test]
async fn the_shot_ends_where_the_engine_ends_it() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_hitscan").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let mut level = read_level(&fixture).await;
    let asks = asks(&level);
    assert!(asks.len() > 1000, "the fan is worth running");

    let ours = ask_server(&fixture, &db, &level, &asks).await;
    let own: Vec<Shot> = asks.iter().map(|ask| level.shoot(ask)).collect();
    check("the shot", &asks, &ours, &own);

    // The same level with the sky over every sector, which is the arm that
    // reaches the branch that spawns nothing. `E1M7` shows the sky over six
    // of its 170 sectors and the fan ends on one of those lines a handful
    // of times.
    level.ceilingpic = vec![level.skyflatnum; level.ceilingpic.len()];
    let ours = ask_server(&fixture, &db, &level, &asks).await;
    let roofless: Vec<Shot> = asks.iter().map(|ask| level.shoot(ask)).collect();
    check("the shot under an open sky", &asks, &ours, &roofless);
    let spared = roofless
        .iter()
        .zip(&own)
        .filter(|(open, own)| open.kind == 0 && own.kind == 1)
        .count();
    assert!(
        spared > 100,
        "the sky spares a wall the shot ended on: {spared}"
    );

    fixture.finish().await;
}

/// Every answer against the reader, and that the fan reached each way a
/// shot can end.
fn check(what: &str, asks: &[Ask], ours: &[Shot], want: &[Shot]) {
    assert_eq!(ours.len(), asks.len());
    for (at, ask) in asks.iter().enumerate() {
        assert_eq!(
            ours[at], want[at],
            "{what} from slot {} at ({}, {}) facing {} at slope {}",
            ask.shooter, ask.x, ask.y, ask.angle, ask.slope
        );
    }
    let of = |kind: u8| want.iter().filter(|shot| shot.kind == kind).count();
    assert!(
        of(0) > 5 && of(1) > 100 && of(2) > 20,
        "{what} ends on nothing, on a line and on a thing: {} {} {}",
        of(0),
        of(1),
        of(2)
    );
    let crossed = want.iter().filter(|shot| !shot.spechit.is_empty()).count();
    assert!(crossed > 20, "{what} crosses special lines: {crossed}");
}

/// The asks: each shooter shooting at the mobjs nearest it and sweeping a
/// full turn, at the bullet slope and at the fixed slopes beside it.
fn asks(level: &Level) -> Vec<Ask> {
    let things = &level.map.things;
    let stride = things.len() / SHOOTERS;
    let mut asks = Vec::new();
    for (slot, from) in things.iter().enumerate().step_by(stride) {
        let ask = |angle: u32, slope: i64| Ask {
            shooter: slot as i64 + 1,
            x: from.x,
            y: from.y,
            z: level.m_z[slot],
            height: level.m_height[slot],
            angle,
            range: MISSILERANGE,
            slope,
        };
        let mut near: Vec<usize> = (0..things.len()).filter(|to| *to != slot).collect();
        near.sort_by_key(|to| (things[*to].x - from.x).abs() + (things[*to].y - from.y).abs());
        let mut angles: Vec<u32> = near
            .into_iter()
            .take(NEAREST)
            .map(|to| pointing(from, &things[to]))
            .collect();
        angles.extend((0..SWEEP).map(|step| (u32::MAX / SWEEP).wrapping_mul(step)));
        for angle in angles {
            asks.push(ask(angle, level.bullet_slope(&ask(angle, 0)).0));
            for slope in SLOPES {
                asks.push(ask(angle, slope));
            }
        }
    }
    asks
}

/// The binary angle from one thing to another.
///
/// Both sides are asked the same integer angle, so this only has to point
/// the fan at something rather than reproduce `R_PointToAngle`.
fn pointing(from: &Thing, to: &Thing) -> u32 {
    let dx = (to.x - from.x) as f64;
    let dy = (to.y - from.y) as f64;
    let turns = dy.atan2(dx) / std::f64::consts::TAU;
    (turns.rem_euclid(1.0) * 4_294_967_296.0) as u32
}

#[derive(Row, Deserialize)]
struct Reached {
    reached: Vec<(u8, i32, i32, i32, i32, Vec<i32>)>,
}

/// What the statement answers for every ask.
async fn ask_server(fixture: &Fixture, db: &str, level: &Level, asks: &[Ask]) -> Vec<Shot> {
    let mut reached: Vec<Shot> = Vec::new();
    // The asks go in batches, because one statement carrying a whole fan
    // is past the server's own `max_query_size`.
    for batch in asks.chunks(BATCH) {
        reached.extend(one_batch(fixture, db, level, batch).await);
    }
    reached
}

async fn one_batch(fixture: &Fixture, db: &str, level: &Level, asks: &[Ask]) -> Vec<Shot> {
    let arrays = level.arrays();
    let targets = shoot::Targets {
        m_x: &arrays.m_x,
        m_y: &arrays.m_y,
        m_z: &arrays.m_z,
        m_radius: &arrays.m_radius,
        m_height: &arrays.m_height,
        m_flags: &arrays.m_flags,
        m_linkseq: &arrays.m_linkseq,
        alive: &arrays.alive,
        floorheight: &arrays.floorheight,
        ceilingheight: &arrays.ceilingheight,
        line_special: &arrays.line_special,
    };
    let list = format!(
        "[{}]",
        asks.iter()
            .map(|ask| shoot::shooting(
                &ask.shooter.to_string(),
                &ask.x.to_string(),
                &ask.y.to_string(),
                &ask.z.to_string(),
                &ask.height.to_string(),
                &ask.angle.to_string(),
                &ask.range.to_string(),
                &ask.slope.to_string(),
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    // The line flags, the line specials and the ceiling flats come from
    // the level, so an arm can change them and both sides read the same
    // arrays.
    let mut constants = maputl::constants(db);
    constants.extend(shoot::constants(db));
    for (name, expr) in &mut constants {
        match name.as_str() {
            "line_flags" => expr.clone_from(&arrays.line_flags),
            "sec_ceilingpic" => expr.clone_from(&arrays.ceilingpic),
            _ => {}
        }
    }
    let sql = format!(
        "WITH\n{},\n    ({list}) AS shot_asks\nSELECT {} AS reached",
        constants
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        shoot::line_attack("shot_asks", &targets),
    );
    let ours: Reached = fixture.scalar(&sql).await;
    ours.reached
        .into_iter()
        .map(|(kind, id, x, y, z, spechit)| Shot {
            kind,
            id: i64::from(id),
            x: i64::from(x),
            y: i64::from(y),
            z: i64::from(z),
            spechit: spechit.into_iter().map(i64::from).collect(),
        })
        .collect()
}
