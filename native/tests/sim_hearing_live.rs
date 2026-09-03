//! `A_Look`'s hearing branch, against a real ClickHouse server.
//!
//! `demo3` leaves every sector's sound target at 0 until the player fires,
//! so the branch is seeded into a state row: the sound target, the deaf
//! flag and a thing that cannot be shot each go in on their own and one
//! tic is run from each.
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

/// One arm per seeded row: where the copy of `BEFORE` lands and which tic
/// runs from it. The tics are far apart so the arms cannot read each
/// other's rows.
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

#[tokio::test]
async fn a_look_wakes_on_what_its_sector_heard() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_hearing").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    // Each arm is the same world with the sound target replaced, so what
    // it leaves differs by exactly what the hearing branch does with it.
    // `lastlook` is seeded away from the player's index in every arm,
    // because a thing that wakes on what it heard never reaches the walk
    // that would put it back.
    let mut statements: Vec<sql::Statement> = Vec::new();
    for (arm, at) in ARMS {
        let mut overrides: Vec<(&str, String)> = vec![(
            "m_lastlook",
            format!("arrayMap(v -> toInt32({SEEDED_LASTLOOK}), p.m_lastlook)"),
        )];
        if arm != "quiet" {
            let target = if arm == "unshootable" {
                // The first thing on the list that cannot be shot, which
                // is what the branch refuses to wake on.
                format!(
                    "indexOf(arrayMap(f -> toUInt8(bitAnd(f, {MF_SHOOTABLE}) = 0), p.m_flags), \
                     toUInt8(1))"
                )
            } else {
                "p.p_mo".to_owned()
            };
            overrides.push((
                "sec_soundtarget",
                format!("arrayMap(v -> toUInt32({target}), p.sec_soundtarget)"),
            ));
        }
        if arm == "deaf" {
            overrides.push((
                "m_flags",
                format!("arrayMap(v -> toInt32(bitOr(v, {MF_AMBUSH})), p.m_flags)"),
            ));
        }
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

    let columns = "tic, prndindex, p_mo, m_target, m_lastlook";
    let wanted: Vec<String> = ARMS.iter().map(|(_, at)| (at + 1).to_string()).collect();
    let rows: Vec<Ran> = fixture
        .rows(&format!(
            "SELECT {columns} FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await;
    let seeded: Vec<Ran> = fixture
        .rows(&format!(
            "SELECT {columns} FROM {db}.native_state WHERE tic = {BEFORE}"
        ))
        .await;
    fixture.finish().await;

    assert_eq!(rows.len(), ARMS.len(), "every arm ran");
    let before = seeded.first().expect("the row the arms copy");
    let at = |arm: &str| {
        let (_, tic) = ARMS.iter().find(|(name, _)| *name == arm).unwrap();
        rows.iter().find(|row| row.tic == tic + 1).unwrap()
    };
    let drew = |arm: &str| at(arm).prndindex.wrapping_sub(before.prndindex);

    let quiet = at("quiet");
    let alert = at("alert");
    assert!(
        before.m_target.iter().all(|target| *target == 0),
        "nothing holds a target before the arms run"
    );
    assert!(quiet.looked() > 0, "the walk over the players runs at all");

    assert!(
        drew("alert") > drew("quiet"),
        "a sector that heard something wakes things a look alone does not: \
         {} draws against {}",
        drew("alert"),
        drew("quiet")
    );
    assert!(
        alert.looked() < quiet.looked(),
        "a thing that wakes on what it heard never reaches the walk over \
         the players: {} walked against {}",
        alert.looked(),
        quiet.looked()
    );
    for slot in 1..=alert.m_target.len() {
        let target = alert.m_target[slot - 1];
        assert!(
            target == 0 || target == alert.p_mo,
            "slot {slot} takes what its sector heard as its target, not {target}"
        );
    }
    assert!(
        targeted(alert, SEEDED_LASTLOOK) > 0,
        "something takes a target without reaching the walk over the players"
    );

    // A deaf thing takes what it heard as its target before it asks
    // whether it can see it, and stays where it is when it cannot.
    let deaf = at("deaf");
    assert!(
        drew("deaf") < drew("alert"),
        "a deaf thing wakes on what it heard only where it can see it: \
         {} draws against {}",
        drew("deaf"),
        drew("alert")
    );
    assert!(
        targeted(deaf, 0) > 0,
        "a deaf thing that cannot see what it heard still holds it and \
         walks the players"
    );

    let unshootable = at("unshootable");
    assert_eq!(
        (
            unshootable.prndindex,
            &unshootable.m_target,
            &unshootable.m_lastlook
        ),
        (quiet.prndindex, &quiet.m_target, &quiet.m_lastlook),
        "a sound target that cannot be shot wakes nothing"
    );
}
