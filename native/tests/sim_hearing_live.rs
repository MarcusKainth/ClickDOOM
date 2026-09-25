//! `A_Look`'s hearing branch, against a real ClickHouse server.
//!
//! `demo3` leaves every sector's sound target at 0 until the player fires,
//! so the branch is seeded into a state row: the sound target, the deaf
//! flag and a thing that cannot be shot each go in on their own and one
//! tic is run from each. Every arm runs in one session, so the suite pays
//! the tic statement's analysis once.
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

use support::arms::{Arm, Arms};
use support::db::Fixture;
use support::resident::Session;

/// The last tic the demo drives. The row it leaves sends more things into
/// `A_Look` than any other tic of the idle window, and none of them has a
/// target yet.
const BEFORE: u32 = 66;

/// `p_mobj.h`
const MF_SHOOTABLE: i32 = 4;
const MF_AMBUSH: i32 = 32;

/// What `lastlook` is seeded to, which is a player index the walk over the
/// players cannot leave behind. One player is in the game, so every way out
/// of that walk leaves 0.
const SEEDED_LASTLOOK: i32 = 3;

/// One arm per seeded row: its name and where the copy of `BEFORE` lands.
const ARMS: [(&str, u32); 4] = [
    ("quiet", 200),
    ("alert", 300),
    ("deaf", 400),
    ("unshootable", 500),
];

#[derive(Row, Deserialize)]
struct Ran {
    tic: u32,
    prndindex: u8,
    p_mo: u32,
    m_target: Vec<u32>,
    m_lastlook: Vec<i32>,
}

impl Ran {
    /// How many things the walk over the players ran for, which is what
    /// puts `lastlook` back to the one player in the game.
    fn looked(&self) -> usize {
        self.m_lastlook.iter().filter(|at| **at == 0).count()
    }
}

/// How many things hold a target and left `lastlook` at `lastlook`.
fn targeted(ran: &Ran, lastlook: i32) -> usize {
    ran.m_target
        .iter()
        .zip(&ran.m_lastlook)
        .filter(|(target, at)| **target != 0 && **at == lastlook)
        .count()
}

/// Each arm is the same world with the sound target replaced, so what it
/// leaves differs by exactly what the hearing branch does with it.
/// `lastlook` is seeded away from the player's index in every arm, because
/// a thing that wakes on what it heard never reaches the walk that would
/// put it back.
fn arms() -> Arms {
    let arms = ARMS
        .into_iter()
        .map(|(name, at)| {
            let mut overrides: Vec<(&str, String)> = vec![(
                "m_lastlook",
                format!("arrayMap(v -> toInt32({SEEDED_LASTLOOK}), p.m_lastlook)"),
            )];
            if name != "quiet" {
                let target = if name == "unshootable" {
                    // The first thing on the list that cannot be shot,
                    // which is what the branch refuses to wake on.
                    format!(
                        "indexOf(arrayMap(f -> toUInt8(bitAnd(f, {MF_SHOOTABLE}) = 0), \
                         p.m_flags), toUInt8(1))"
                    )
                } else {
                    "p.p_mo".to_owned()
                };
                overrides.push((
                    "sec_soundtarget",
                    format!("arrayMap(v -> toUInt32({target}), p.sec_soundtarget)"),
                ));
            }
            if name == "deaf" {
                overrides.push((
                    "m_flags",
                    format!("arrayMap(v -> toInt32(bitOr(v, {MF_AMBUSH})), p.m_flags)"),
                ));
            }
            Arm {
                name,
                from: BEFORE,
                overrides,
                at,
                inputs: vec![Input::keys(at + 1, 0, (0, 0))],
            }
        })
        .collect();
    Arms::new((1..=BEFORE).map(Input::demo).collect(), arms)
}

#[tokio::test]
async fn a_look_wakes_on_what_its_sector_heard() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_hearing").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let arms = arms();
    let mut session = Session::open(&fixture, false).await;
    arms.drive(&fixture, &mut session).await;
    session.close().await;

    let columns = "tic, prndindex, p_mo, m_target, m_lastlook";
    let mut ran: Vec<Vec<Ran>> = Vec::new();
    for arm in arms.all() {
        ran.push(arm.rows(&fixture, "native_state", columns).await);
    }
    fixture.finish().await;

    for (arm, rows) in arms.all().iter().zip(&ran) {
        let tics: Vec<u32> = rows.iter().map(|row| row.tic).collect();
        assert_eq!(
            tics,
            Vec::from_iter(arm.tics()),
            "arm {} left its seeded row and the tic run from it",
            arm.name
        );
    }
    // Every seeded row carries the walked row's own index and targets,
    // since no arm replaces them.
    let pair = |name: &str| {
        let rows = &ran[arms.all().iter().position(|arm| arm.name == name).unwrap()];
        (&rows[0], &rows[1])
    };
    let at = |name: &str| pair(name).1;
    let drew = |name: &str| {
        let (seeded, after) = pair(name);
        after.prndindex.wrapping_sub(seeded.prndindex)
    };
    let mut checks = arms.checks();

    let (before, quiet) = pair("quiet");
    let alert = at("alert");
    let mut arm = checks.arm("quiet");
    arm.check(
        before.m_target.iter().all(|target| *target == 0),
        "nothing holds a target before the arms run",
    );
    arm.check(quiet.looked() > 0, "the walk over the players runs at all");

    let mut arm = checks.arm("alert");
    arm.check(
        drew("alert") > drew("quiet"),
        format_args!(
            "a sector that heard something wakes things a look alone does not: \
             {} draws against {}",
            drew("alert"),
            drew("quiet")
        ),
    );
    arm.check(
        alert.looked() < quiet.looked(),
        format_args!(
            "a thing that wakes on what it heard never reaches the walk over \
             the players: {} walked against {}",
            alert.looked(),
            quiet.looked()
        ),
    );
    let strays: Vec<(usize, u32)> = (1..)
        .zip(alert.m_target.iter().copied())
        .filter(|(_, target)| *target != 0 && *target != alert.p_mo)
        .collect();
    arm.eq(
        strays,
        Vec::new(),
        "the (slot, target) pairs that take something other than what their \
         sector heard as their target",
    );
    arm.check(
        targeted(alert, SEEDED_LASTLOOK) > 0,
        "something takes a target without reaching the walk over the players",
    );

    // A deaf thing takes what it heard as its target before it asks
    // whether it can see it, and stays where it is when it cannot.
    let deaf = at("deaf");
    let mut arm = checks.arm("deaf");
    arm.check(
        drew("deaf") < drew("alert"),
        format_args!(
            "a deaf thing wakes on what it heard only where it can see it: \
             {} draws against {}",
            drew("deaf"),
            drew("alert")
        ),
    );
    arm.check(
        targeted(deaf, 0) > 0,
        "a deaf thing that cannot see what it heard still holds it and \
         walks the players",
    );

    let unshootable = at("unshootable");
    checks.arm("unshootable").eq(
        (
            unshootable.prndindex,
            &unshootable.m_target,
            &unshootable.m_lastlook,
        ),
        (quiet.prndindex, &quiet.m_target, &quiet.m_lastlook),
        "a sound target that cannot be shot wakes nothing",
    );
    checks.finish();
}
