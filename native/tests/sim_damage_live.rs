//! `P_DamageMobj` and `P_KillMobj` against a real ClickHouse server.
//!
//! What a hit does is what the seven shots of `A_FireShotgun` end on, so it
//! is checked on its own against `native/tests/support/damage.rs`, a reader
//! written from `p_inter.c`. The fan seeds a world of the engine's own
//! thing types at healths either side of what a shot does and hits every
//! one of them from every inflictor and source pair.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim::{self, inter};
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::damage::{Hurt, Mobj, World};
use support::db::Fixture;
use support::mobj::thing_type;

const FRACUNIT: i64 = 1 << 16;
/// `p_mobj.h`
const MF_SKULLFLY: i64 = 0x100_0000;
const MF_NOCLIP: i64 = 0x1000;
const MF_JUSTHIT: i64 = 64;
/// `doomdef.h`
const WP_SHOTGUN: i64 = 2;
const WP_CHAINSAW: i64 = 7;

/// The types the fan hits: the three that drop something, one that does
/// not, the two the routine names by hand, and a barrel.
const KINDS: [&str; 7] = [
    "MT_POSSESSED",
    "MT_SHOTGUY",
    "MT_CHAINGUY",
    "MT_TROOP",
    "MT_SKULL",
    "MT_VILE",
    "MT_BARREL",
];

/// What a gunshot can do, and enough to overkill a zombieman twice over.
const DAMAGE: [i64; 5] = [5, 10, 15, 30, 200];
/// The random indices the fan hits from.
const INDICES: [i64; 3] = [0, 137, 253];

#[tokio::test]
async fn a_hit_leaves_what_the_engine_leaves() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_damage").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let (mobjs, sources) = mobjs();
    let asks = asks(&mobjs, &sources);
    assert!(asks.len() > 200, "the fan is worth running");
    for weapon in [WP_SHOTGUN, WP_CHAINSAW] {
        for prnd in INDICES {
            let world = World {
                mobjs: mobjs.clone(),
                prndindex: prnd,
                readyweapon: weapon,
            };
            let ours = ask_server(&fixture, &db, &world, &asks).await;
            let want: Vec<Hurt> = asks
                .iter()
                .map(|(t, i, s, d, base)| world.damage(*t, *i, *s, *d, *base))
                .collect();
            let what = format!("a hit from index {prnd} with weapon {weapon}");
            assert_eq!(ours.len(), want.len());
            for (at, hurt) in want.iter().enumerate() {
                assert_eq!(&ours[at], hurt, "{what}, ask {at}: {:?}", asks[at]);
            }
            check(&what, &want, &mobjs, &asks);
            a_pained_thing_stays_in_its_pain_frame(&what, &mobjs, &asks, &ours, prnd);
        }
    }
    fixture.finish().await;
}

/// That the fan reached every arm the routine has.
fn check(what: &str, want: &[Hurt], mobjs: &[Mobj], asks: &[Ask]) {
    let count = |of: &dyn Fn(&Hurt) -> bool| want.iter().filter(|h| of(h)).count();
    let killed = count(&|h| h.killed);
    let pained = count(&|h| !h.killed && h.flags & 64 != 0);
    let dropped = count(&|h| h.drop != -1);
    let chased = count(&|h| h.threshold == 100);
    let stuck = count(&|h| h.stuck);
    // Stuck because the frame the call entered carries a routine this does
    // not run, rather than because the target is a player.
    let routine = want
        .iter()
        .enumerate()
        .filter(|(at, h)| h.stuck && mobjs[(asks[*at].0 - 1) as usize].player == -1)
        .count();
    let quiet = count(&|h| h.draws == 0);
    let fell = count(&|h| h.draws == 2);
    // The two slots pull apart only where they differ, so the fan has to
    // reach a push with no source behind it and a source that pushes
    // nothing.
    // A charging thing stops dead wherever the hit lands, so its
    // momentum moves without a push and it is left out.
    let pushed = |at: usize| {
        let it = &mobjs[(asks[at].0 - 1) as usize];
        it.flags & MF_SKULLFLY == 0 && (want[at].momx != it.momx || want[at].momy != it.momy)
    };
    let credited_only = (0..want.len())
        .filter(|at| asks[*at].1 == 0 && asks[*at].2 != 0 && pushed(*at))
        .count();
    let uncredited = (0..want.len())
        .filter(|at| asks[*at].1 != 0 && asks[*at].2 == 0 && pushed(*at))
        .count();
    let crossed = (0..want.len())
        .filter(|at| asks[*at].1 != 0 && asks[*at].2 != 0 && asks[*at].1 != asks[*at].2)
        .filter(|at| pushed(*at))
        .count();
    assert!(
        killed > 10
            && pained > 10
            && dropped > 5
            && chased > 10
            && stuck > 0
            && routine > 5
            && quiet > 0
            && fell > 0
            && credited_only == 0
            && uncredited > 10
            && crossed > 10,
        "{what} reaches every arm: killed {killed}, pained {pained}, dropped {dropped}, \
         chased {chased}, stuck {stuck}, routine {routine}, quiet {quiet}, fell {fell}, \
         credited_only {credited_only}, uncredited {uncredited}, crossed {crossed}"
    );
}

/// A thing hit in its spawn frame whose pain draw succeeds ends in its
/// pain frame, not its see frame.
///
/// `P_DamageMobj` enters the pain frame before it decides whether to send
/// the thing after what hit it, and the decision reads the frame the thing
/// is in by then. This asserts the outcome rather than comparing against
/// the reader, because the two shared the same misreading.
fn a_pained_thing_stays_in_its_pain_frame(
    what: &str,
    mobjs: &[Mobj],
    asks: &[Ask],
    ours: &[Hurt],
    prnd: i64,
) {
    let rnd = tables::table("rndtable").unwrap().ints("value").unwrap();
    let info = |kind: i64, column: &str| {
        tables::table("mobjinfo").unwrap().ints(column).unwrap()[kind as usize]
    };
    let mut pained = 0;
    for (at, (target, _, source, damage, base)) in asks.iter().enumerate() {
        let it = &mobjs[(*target - 1) as usize];
        // A thing at full health cannot be knocked over, so its one draw
        // is the pain chance and it sits at the base.
        let full = it.health == info(it.kind, "spawnhealth");
        let spawned = it.state == info(it.kind, "spawnstate");
        let lives = *damage < it.health;
        let awake = info(it.kind, "seestate") != 0;
        if *source == 0 || it.player != -1 || !full || !spawned || !lives || !awake {
            continue;
        }
        if rnd[((prnd + base + 1) & 255) as usize] >= info(it.kind, "painchance") {
            continue;
        }
        assert_eq!(
            ours[at].state,
            info(it.kind, "painstate"),
            "{what}: a thing pained in its spawn frame ends there, ask {at}"
        );
        assert!(
            ours[at].flags & MF_JUSTHIT != 0,
            "{what}: a pained thing fights back, ask {at}"
        );
        pained += 1;
    }
    assert!(
        pained > 5,
        "{what}: the fan pains a thing in its spawn frame: {pained}"
    );
}

/// The world the fan hits and the slots that do the hitting: one of each
/// type at full health, one of each with a sliver left, one of each
/// already chasing something else, one of each in its own pain frame, a
/// charging lost soul, a corpse, a thing that cannot be pushed, a player,
/// and the two sources.
fn mobjs() -> (Vec<Mobj>, Vec<i64>) {
    let info = |kind: &str, column: &str| {
        let at = thing_type(kind) as usize;
        tables::table("mobjinfo").unwrap().ints(column).unwrap()[at]
    };
    let mut mobjs: Vec<Mobj> = Vec::new();
    let mut add = |kind: &str, health: i64, z: i64, flags: i64, player: i64, threshold: i64| {
        mobjs.push(Mobj {
            x: 100 * FRACUNIT + (mobjs.len() as i64) * 40 * FRACUNIT,
            y: 200 * FRACUNIT - (mobjs.len() as i64) * 24 * FRACUNIT,
            z,
            momx: 0,
            momy: 0,
            momz: 0,
            kind: thing_type(kind),
            state: info(kind, "spawnstate"),
            tics: 7,
            flags: info(kind, "flags") | flags,
            health,
            height: info(kind, "height"),
            target: 0,
            threshold,
            player,
        });
    };
    for kind in KINDS {
        add(kind, info(kind, "spawnhealth"), 200 * FRACUNIT, 0, -1, 0);
        add(kind, 4, 200 * FRACUNIT, 0, -1, 0);
        // One already chasing something else, which is what the threshold
        // holds a thing to.
        add(kind, info(kind, "spawnhealth"), 200 * FRACUNIT, 0, -1, 30);
    }

    // A lost soul charging, one charging that is already dead, a corpse, a
    // thing that cannot be pushed, a player, and the two sources.
    add("MT_SKULL", 60, 200 * FRACUNIT, MF_SKULLFLY, -1, 0);
    add("MT_SKULL", 0, 200 * FRACUNIT, MF_SKULLFLY, -1, 0);
    add("MT_TROOP", 0, 200 * FRACUNIT, 0, -1, 0);
    add("MT_TROOP", 60, 200 * FRACUNIT, MF_NOCLIP, -1, 0);
    add("MT_PLAYER", 100, 200 * FRACUNIT, 0, 0, 0);
    add("MT_PLAYER", 100, 100 * FRACUNIT, 0, 0, 0);
    add("MT_VILE", 700, 100 * FRACUNIT, 0, -1, 0);
    // A charging thing carries momentum, which is what the hit stops. A
    // hit that does not land keeps it, so the two charging entries are the
    // only ones the guard can be read on.
    for it in &mut mobjs {
        if it.flags & MF_SKULLFLY != 0 {
            it.momx = 7 * FRACUNIT;
            it.momy = -5 * FRACUNIT;
            it.momz = 3 * FRACUNIT;
        }
    }
    // The two that do the hitting, both standing well below their targets:
    // the player, whom a chainsaw holds back, and the one it does not.
    let sources = vec![0, mobjs.len() as i64 - 1, mobjs.len() as i64];
    // One of each already standing in its own pain frame, which is where a
    // hit enters the frame the thing is already in.
    let painful: Vec<Mobj> = KINDS
        .iter()
        .map(|kind| thing_type(kind))
        .filter(|kind| painstate(*kind) != 0)
        .map(|kind| {
            let mut it = mobjs
                .iter()
                .find(|m| m.kind == kind)
                .expect("the world holds one of each")
                .clone();
            it.state = painstate(kind);
            it.x += 4000 * FRACUNIT;
            it
        })
        .collect();
    mobjs.extend(painful);
    (mobjs, sources)
}

/// `mobjinfo`'s own pain frame for a type, by its id.
fn painstate(kind: i64) -> i64 {
    tables::table("mobjinfo")
        .unwrap()
        .ints("painstate")
        .unwrap()[kind as usize]
}

/// One ask: the target, the inflictor, the source, the damage, and how
/// many numbers the tic drew before it.
type Ask = (i64, i64, i64, i64, i64);

/// Every target hit from every inflictor and source pair at every damage,
/// each at a base of its own so no two share their draws.
///
/// The pairs are every combination of the slots, so the fan covers a hit
/// with no inflictor, one with no source, and the crossed pairs a missile
/// makes.
fn asks(mobjs: &[Mobj], sources: &[i64]) -> Vec<Ask> {
    let mut asks = Vec::new();
    for target in 1..=mobjs.len() as i64 {
        for inflictor in sources {
            for source in sources {
                for damage in DAMAGE {
                    asks.push((target, *inflictor, *source, damage, asks.len() as i64 % 19));
                }
            }
        }
    }
    asks
}

/// One answer as the statement gives it, in [`inter::hurt`]'s order.
type HurtRow = (
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
    i32,
    u8,
    u8,
    i32,
    u32,
    u8,
);

#[derive(Row, Deserialize)]
struct Hurts {
    hurts: Vec<HurtRow>,
}

fn literal(of: &[i64]) -> String {
    format!(
        "[{}]",
        of.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
    )
}

async fn ask_server(fixture: &Fixture, db: &str, world: &World, asks: &[Ask]) -> Vec<Hurt> {
    let of = |get: &dyn Fn(&Mobj) -> i64| literal(&world.mobjs.iter().map(get).collect::<Vec<_>>());
    let (m_x, m_y, m_z) = (of(&|m| m.x), of(&|m| m.y), of(&|m| m.z));
    let (m_momx, m_momy, m_momz) = (of(&|m| m.momx), of(&|m| m.momy), of(&|m| m.momz));
    let (m_type, m_state, m_tics) = (of(&|m| m.kind), of(&|m| m.state), of(&|m| m.tics));
    let (m_flags, m_health, m_height) = (of(&|m| m.flags), of(&|m| m.health), of(&|m| m.height));
    let (m_target, m_threshold, m_player) =
        (of(&|m| m.target), of(&|m| m.threshold), of(&|m| m.player));
    let m_reactiontime = of(&|_| 0);
    let (prnd, weapon) = (world.prndindex.to_string(), world.readyweapon.to_string());
    let hurting = inter::Hurting {
        m_x: &m_x,
        m_y: &m_y,
        m_z: &m_z,
        m_momx: &m_momx,
        m_momy: &m_momy,
        m_momz: &m_momz,
        m_reactiontime: &m_reactiontime,
        m_type: &m_type,
        m_state: &m_state,
        m_tics: &m_tics,
        m_flags: &m_flags,
        m_health: &m_health,
        m_height: &m_height,
        m_target: &m_target,
        m_threshold: &m_threshold,
        m_player: &m_player,
        prndindex: &prnd,
        readyweapon: &weapon,
    };
    let list = format!(
        "[{}]",
        asks.iter()
            .map(|(t, i, s, d, base)| format!(
                "(toUInt32({t}), toUInt32({i}), toUInt32({s}), toInt32({d}), toUInt32({base}))"
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut constants = sim::constants(db);
    constants.extend(inter::damage_constants(db));
    let sql = format!(
        "WITH\n{},\n    ({list}) AS dm_asks\nSELECT {} AS hurts",
        constants
            .into_iter()
            .map(|(name, expr)| format!("    ({expr}) AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        inter::damage_mobj("dm_asks", &hurting),
    );
    let ours: Hurts = fixture.scalar(&sql).await;
    ours.hurts
        .into_iter()
        .map(|h| Hurt {
            health: i64::from(h.0),
            flags: i64::from(h.1),
            state: i64::from(h.2),
            tics: i64::from(h.3),
            momx: i64::from(h.4),
            momy: i64::from(h.5),
            momz: i64::from(h.6),
            height: i64::from(h.7),
            reactiontime: i64::from(h.8),
            target: i64::from(h.9),
            threshold: i64::from(h.10),
            killed: h.11 == 1,
            counted: h.12 == 1,
            drop: i64::from(h.13),
            draws: i64::from(h.14),
            stuck: h.15 == 1,
        })
        .collect()
}
