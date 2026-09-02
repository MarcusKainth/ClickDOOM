//! The tic transform against the reference emulator, on a real server.
//!
//! Two things are checked. Every field the committed probe fixture covers
//! has to agree with the engine, apart from a named set the simulation
//! does not compute. Then the player walks into the wall demo3 puts
//! in front of it, and the position and momentum `P_SlideMove` leaves are
//! checked against the engine's own.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use std::path::Path;

use clickdoom_native::sql::{parity, probe, sim};
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;

/// The engine's state at the frame commits the fixture keeps, which are
/// the last frame of gametics 2, 3, 4 and 963.
fn fixture_tsv() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../refemu/probe/fixtures/demo3-frames.9a6a47d01119.tsv");
    std::fs::read_to_string(path).expect("the probe fixture is committed")
}

/// Fields the simulation does not compute, with the group they sit in.
///
/// Each one waits on a thinker: the mobj state cycle behind `m_state`,
/// `A_Look` behind `m_lastlook`, and `P_MovePsprites` behind `psp_sy`.
/// A field that starts differing and is not named here fails the test, and
/// so does one named here that agrees, so the list cannot outlive what it
/// excuses.
const OPEN: [&str; 5] = ["m_frame", "m_tics", "m_state", "m_lastlook", "psp_sy"];

/// How far the walk runs. Gametic 32 is where demo3 first puts a wall in
/// the way, the tics after it are the slide along that wall, and the door
/// the press at 73 opens has reached the top and left the list by 120.
const WALK_TICS: u32 = 120;

/// `p_local.h`: the use key's bit in a tic command.
const BT_USE: u8 = 2;

/// `gametic, thinkers, sec_ceilingheight[63], sec_specialdata[63],
/// line_special[951]` around the door demo3 opens, read out of the
/// reference emulator's trace. The press at 73 makes the thinker and
/// spends the line's special, the ceiling rises a step a tic, and the
/// thinker comes off the list on the tic it reaches the top.
const DOOR: [(u32, usize, i32, u32, i16); 6] = [
    (72, 16, 0, 0, 31),
    (73, 17, 131072, 17, 0),
    (80, 17, 1048576, 17, 0),
    (106, 17, 4456448, 17, 0),
    (107, 16, 4456448, 0, 0),
    (120, 16, 4456448, 0, 0),
];

/// A tic the use key goes down on and the press reaches nothing special.
/// The engine plays a sound and moves on, and so does the simulation.
const USE_INTO_NOTHING: u32 = 42;

/// `gametic, m_x, m_y, m_momx, m_momy` for the player, read out of the
/// reference emulator's demo3 trace. Gametic 31 is the last free move, 32
/// is the blocked one, and the rest are the slide.
const WALK: [(u32, i32, i32, i32, i32); 6] = [
    (2, 6225766, 34419194, -28303, -79195),
    (31, 4639872, 25605989, 78408, -506265),
    (32, 4756160, 25182290, 17337, 0),
    (33, 4847534, 25166570, 19647, 0),
    (34, 4901210, 25166337, 47338, 0),
    (40, 5126423, 25166337, 26222, 0),
];

#[derive(Row, Deserialize)]
struct Divergence {
    field: String,
    kind: String,
    /// How many of the compared tics differ in this field.
    tics: u64,
    first_tic: u32,
    slot: u32,
    ours: String,
    theirs: String,
}

#[derive(Row, Deserialize)]
struct Walked {
    tic: u32,
    x: i32,
    y: i32,
    momx: i32,
    momy: i32,
    unresolved: u8,
    buttons: u8,
    thinkers: u64,
    ceiling: i32,
    specialdata: u32,
    special: i16,
}

async fn walked(fixture: &Fixture, db: &str) -> Vec<Walked> {
    fixture
        .rows(&format!(
            "SELECT tic, m_x[p_mo] AS x, m_y[p_mo] AS y, \
             m_momx[p_mo] AS momx, m_momy[p_mo] AS momy, \
             unresolved, toUInt8(p_cmd_buttons) AS buttons, \
             toUInt64(length(s_kind)) AS thinkers, sec_ceilingheight[63] AS ceiling, \
             sec_specialdata[63] AS specialdata, line_special[951] AS special \
             FROM {db}.native_state ORDER BY tic"
        ))
        .await
}

#[tokio::test]
async fn the_tic_matches_the_engine_where_the_fixture_reaches() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_parity").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.push(probe::schema_statement(&db));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, WALK_TICS));
    plan.push(probe::insert(&db, &fixture_tsv()).unwrap());
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let summary: Vec<Divergence> = fixture.rows(&parity::field_summary(&db)).await;
    let walk = walked(&fixture, &db).await;
    fixture.finish().await;

    let differ: Vec<&str> = summary.iter().map(|d| d.field.as_str()).collect();
    let unexpected: Vec<&Divergence> = summary
        .iter()
        .filter(|d| !OPEN.contains(&d.field.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "fields that no longer match the engine: {}",
        unexpected
            .iter()
            .map(|d| format!(
                "{} ({}) differs on {} tics, first at {} slot {}, ours {} theirs {}",
                d.field, d.kind, d.tics, d.first_tic, d.slot, d.ours, d.theirs
            ))
            .collect::<Vec<_>>()
            .join("; ")
    );
    // The fixture has to reach far enough for the open fields to show, or
    // the run above proves nothing about the ones that are not open.
    assert_eq!(
        differ.len(),
        OPEN.len(),
        "the fixture no longer covers every open field, so the check is \
         weaker than it reads: {differ:?}"
    );

    // The setup row the level leaves behind is tic 0, so the run holds
    // one row more than the tics it was asked for.
    assert_eq!(walk.len(), WALK_TICS as usize + 1, "every tic ran");

    let at = |tic: u32| {
        walk.iter()
            .find(|row| row.tic == tic)
            .unwrap_or_else(|| panic!("gametic {tic} ran"))
    };
    // The run reaches past the door and every tic of it completes. The
    // first the simulation cannot finish is gametic 380, a use press that
    // reaches a switch, which is past the end of this run.
    for row in &walk {
        assert_eq!(
            row.unresolved, 0,
            "gametic {} was not carried through",
            row.tic
        );
    }
    let pressed = at(USE_INTO_NOTHING);
    assert_eq!(pressed.buttons & BT_USE, BT_USE, "the use key is down");
    assert_eq!(
        pressed.unresolved, 0,
        "a press that reaches no special line finishes like any other tic"
    );
    for (tic, thinkers, ceiling, specialdata, special) in DOOR {
        let row = at(tic);
        assert_eq!(
            (row.thinkers, row.ceiling, row.specialdata, row.special),
            (thinkers as u64, ceiling, specialdata, special),
            "the door at gametic {tic}"
        );
    }

    for (tic, x, y, momx, momy) in WALK {
        let at = at(tic);
        assert_eq!(
            (at.x, at.y, at.momx, at.momy),
            (x, y, momx, momy),
            "gametic {tic}"
        );
    }
}
