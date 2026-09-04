//! `P_AimLineAttack` and `P_BulletSlope` against a real ClickHouse server.
//!
//! The aim is a fold over what the blockmap walk crossed, so it is checked
//! on its own against `native/tests/support/shoot.rs`, a reader written from
//! `p_map.c` and `p_pspr.c`. The fan is the mobjs `P_SetupLevel` leaves on
//! `E1M7` aiming at each other over the level's own geometry, which is
//! what a shot asks.
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
use support::shoot::{Ask, Level, read_level};
use support::traverse::Thing;

const FRACUNIT: i64 = 1 << 16;
/// `p_local.h`: how far a hitscan reaches.
const MISSILERANGE: i64 = 32 * 64 * FRACUNIT;
/// `doomdata.h`
const ML_TWOSIDED: i64 = 4;
/// `p_pspr.c`: what `P_BulletSlope` looks with.
const AIMRANGE: i64 = 16 * 64 * FRACUNIT;
const AIMSWING: u32 = 1 << 26;

/// How many of the level's mobjs shoot, spread evenly through the list,
/// and how many of the ones nearest each it aims at.
const SHOOTERS: usize = 16;
const NEAREST: usize = 8;
/// How many angles of a full turn each shooter also sweeps, which is what
/// reaches the walls and the empty air.
const SWEEP: u32 = 16;

#[tokio::test]
async fn the_aim_finds_what_the_engine_finds() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_aim").await;
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
    assert!(asks.len() > 300, "the fan is worth running");

    let ours = ask_server(&fixture, &db, &level, &asks, false).await;
    let own: Vec<(i64, i64)> = asks.iter().map(|ask| level.aim(ask)).collect();
    check("the aim", &asks, &ours, &own);

    let theirs = ask_server(&fixture, &db, &level, &asks, true).await;
    let swung: Vec<(i64, i64)> = asks.iter().map(|ask| level.bullet_slope(ask)).collect();
    check("the bullet slope", &asks, &theirs, &swung);
    swing(&level, &asks, &swung);

    // The same level with every sector brought to one floor and one
    // ceiling, which is the arm that reaches the traverser's test for a
    // line whose two sides carry the same height: there the engine leaves
    // the window alone, and every mobj that stands off the flat floor is
    // one a trace that narrowed anyway would lose.
    flatten(&mut level);
    let ours = ask_server(&fixture, &db, &level, &asks, false).await;
    let flat: Vec<(i64, i64)> = asks.iter().map(|ask| level.aim(ask)).collect();
    check("the aim over one floor", &asks, &ours, &flat);
    let moved = flat.iter().zip(&own).filter(|(a, b)| a != b).count();
    assert!(moved > 20, "the flat world asks something else: {moved}");
    let theirs = ask_server(&fixture, &db, &level, &asks, true).await;
    let want: Vec<(i64, i64)> = asks.iter().map(|ask| level.bullet_slope(ask)).collect();
    check("the bullet slope over one floor", &asks, &theirs, &want);
    swing(&level, &asks, &want);

    // `E1M7` carries no line with a second side and no `ML_TWOSIDED`, so
    // the traverser's own first test is unreachable over the level as it
    // stands. Clearing the flag on every other line puts it back.
    let barred = bar_some_lines(&mut level);
    assert!(barred > 40, "the arm bars enough lines: {barred}");
    let ours = ask_server(&fixture, &db, &level, &asks, false).await;
    let want: Vec<(i64, i64)> = asks.iter().map(|ask| level.aim(ask)).collect();
    check("the aim past a barred line", &asks, &ours, &want);
    let moved = want.iter().zip(&flat).filter(|(a, b)| a != b).count();
    assert!(moved > 10, "the barred lines stop something: {moved}");

    fixture.finish().await;
}

/// That the swing either side of the thing's own angle decided something:
/// an aim it found that the one straight ahead missed, and one where the
/// two sides found different targets, which is what the order settles.
fn swing(level: &Level, asks: &[Ask], found: &[(i64, i64)]) {
    let mut rescued = 0;
    let mut parted = 0;
    for (at, ask) in asks.iter().enumerate() {
        if found[at].1 != 0 && level.aim(ask).1 == 0 {
            rescued += 1;
        }
        let side = |by: u32| {
            level.aim(&Ask {
                angle: ask.angle.wrapping_add(by),
                range: AIMRANGE,
                ..*ask
            })
        };
        let (left, right) = (side(AIMSWING), side(AIMSWING.wrapping_neg()));
        if left.1 != 0 && right.1 != 0 && left != right {
            parted += 1;
        }
    }
    assert!(rescued > 0, "the swing finds what the aim ahead misses");
    assert!(parted > 0, "the two sides of the swing part: {parted}");
}

/// Every sector brought to the level's highest floor, with a room's height
/// of ceiling over it.
fn flatten(level: &mut Level) {
    let floor = level.floorheight.iter().copied().max().unwrap_or_default();
    level.floorheight = vec![floor; level.floorheight.len()];
    level.ceilingheight = vec![floor + 128 * FRACUNIT; level.ceilingheight.len()];
}

/// `ML_TWOSIDED` cleared on every other two-sided line, and how many that
/// is. Their second side stays, so only the flag stops a trace.
fn bar_some_lines(level: &mut Level) -> usize {
    let mut barred = 0;
    for line in 0..level.line_flags.len() {
        if level.line_side1[line] != -1 && line % 2 == 0 {
            level.line_flags[line] &= !ML_TWOSIDED;
            barred += 1;
        }
    }
    barred
}

/// Every answer against the reader, and that the fan reached both arms.
fn check(what: &str, asks: &[Ask], ours: &[(i64, i64)], want: &[(i64, i64)]) {
    assert_eq!(ours.len(), asks.len());
    for (at, ask) in asks.iter().enumerate() {
        assert_eq!(
            ours[at], want[at],
            "{what} from slot {} at ({}, {}) facing {}",
            ask.shooter, ask.x, ask.y, ask.angle
        );
    }
    let hit = want.iter().filter(|found| found.1 != 0).count();
    assert!(
        hit > 20 && hit + 20 < want.len(),
        "{what} has to find things and miss them: {hit} of {}",
        want.len()
    );
    let sloped = want.iter().filter(|found| found.0 != 0).count();
    assert!(sloped > 10, "{what} answers a slope of its own: {sloped}");
}

/// The asks: each shooter aiming at the mobjs nearest it, and sweeping a
/// full turn.
fn asks(level: &Level) -> Vec<Ask> {
    let things = &level.map.things;
    let stride = things.len() / SHOOTERS;
    let mut asks = Vec::new();
    for (slot, from) in things.iter().enumerate().step_by(stride) {
        let ask = |angle: u32| Ask {
            shooter: slot as i64 + 1,
            x: from.x,
            y: from.y,
            z: level.m_z[slot],
            height: level.m_height[slot],
            angle,
            range: MISSILERANGE,
            slope: 0,
        };
        let mut near: Vec<usize> = (0..things.len()).filter(|to| *to != slot).collect();
        near.sort_by_key(|to| (things[*to].x - from.x).abs() + (things[*to].y - from.y).abs());
        for to in near.into_iter().take(NEAREST) {
            asks.push(ask(pointing(from, &things[to])));
        }
        for step in 0..SWEEP {
            asks.push(ask((u32::MAX / SWEEP).wrapping_mul(step)));
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

/// One traverse answer as the statement gives it, in [`shoot::reached`]'s
/// order.
type ReachedRow = (i32, i32, u8, i32, i32, i32, i32, Vec<i32>);

#[derive(Row, Deserialize)]
struct Found {
    found: Vec<ReachedRow>,
}

/// What the statement answers for every ask.
async fn ask_server(
    fixture: &Fixture,
    db: &str,
    level: &Level,
    asks: &[Ask],
    swing: bool,
) -> Vec<(i64, i64)> {
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
            .map(|ask| shoot::asking(
                &ask.shooter.to_string(),
                &ask.x.to_string(),
                &ask.y.to_string(),
                &ask.z.to_string(),
                &ask.height.to_string(),
                &ask.angle.to_string(),
                &ask.range.to_string(),
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let answer = if swing {
        shoot::bullet_slope("aim_asks", &targets)
    } else {
        shoot::traverse("aim_asks", &targets)
    };
    // The line flags come from the level, so an arm can change them and
    // both sides read the same array.
    let mut constants = maputl::constants(db);
    constants.extend(shoot::constants(db));
    for (name, expr) in &mut constants {
        if name == "line_flags" {
            expr.clone_from(&arrays.line_flags);
        }
    }
    let sql = format!(
        "WITH\n{},\n    ({list}) AS aim_asks\nSELECT {answer} AS found",
        constants
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
    );
    let ours: Found = fixture.scalar(&sql).await;
    ours.found
        .into_iter()
        .map(|found| (i64::from(found.0), i64::from(found.1)))
        .collect()
}
