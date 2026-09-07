//! `P_XYMovement` for a thing that is not the player, against a real
//! ClickHouse server.
//!
//! `demo3` only ever gives a monster the momentum one shotgun pellet's
//! `P_DamageMobj` leaves, which is small enough to spend in one part, so
//! the move a monster makes there is checked in `sim_parity_live` against
//! the engine's own trace. The parts of the routine the demo does not
//! reach are seeded here: the halving a momentum over half of `MAXMOVE`
//! takes, the clamp above `MAXMOVE`, and the special line a move crosses.
//!
//! Every arm is a row seeded into one session. A session pays the tic
//! statement's analysis once, and for a suite this size that is the whole
//! of what it costs.
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

/// The tic every arm copies its row from. Gametic 40 is early enough that
/// no monster has woken and the list still holds the level's own things.
const BEFORE: u32 = 40;

/// The slot the momentum is seeded onto. Slot 118 is the monster the
/// demo's player later shoots, and at gametic 40 it stands still.
const SLOT: usize = 118;

/// A second slot the crowd arm seeds beside [`SLOT`], also alive and
/// standing still at gametic 40.
const SLOT2: usize = 258;

/// How far apart the crowd arm's two things stand: comfortably more than
/// twice [`NARROW`], so their own boxes never come close to touching, and
/// comfortably less than the general movers' own gate once it pads either
/// thing's radius with its speed and its momentum.
const CROWD_APART: i64 = 5 * 65_536;

/// The momentum the crowd arm gives both things, along the axis neither
/// stands apart on, so the gap between them never changes.
const CROWD_MOMY: i64 = 2 * 65_536;

/// Where an arm that needs room puts the thing before it moves, and how
/// wide it is made there.
///
/// A move of `MAXMOVE` covers thirty map units, which is further than the
/// monster can go from where it stands. This is where the demo's player
/// stands at gametic 26, part way down a corridor it walks the whole of,
/// so the way back up is open for as far as the move reaches. The thing is
/// given a radius of one unit as well, so what the corridor is shaped like
/// cannot decide what this reads; the width is no part of the arithmetic
/// under test.
const OPEN: (i64, i64) = (4_498_858, 28_739_495);
const NARROW: i64 = 65_536;

/// Eight units under line 50, which runs level along `y` for two hundred
/// and fifty units with the same floor and ceiling either side of it and
/// nothing blocking.
const BELOW_A_LINE: (i64, i64) = (12_582_912, -1_048_576 - 8 * 65_536);

/// `p_mobj.c`
const MAXMOVE: i64 = 30 << 16;
const STOPSPEED: i64 = 0x1000;
const FRICTION: i64 = 0xe800;

/// A momentum over half of `MAXMOVE` on one axis, which is what makes the
/// engine spend the move in two parts, and an odd negative one on the
/// other, which the two halves round differently. The test on the split
/// only means anything while the axis carrying it is the one with room,
/// so the large one is the axis the corridor runs along.
const SPLIT_MOMY: i64 = MAXMOVE / 2 + 1;
const SPLIT_MOMX: i64 = -3;

/// A momentum over `MAXMOVE`, which the clamp cuts down before anything
/// else reads it.
const OVER_MOMY: i64 = MAXMOVE + 100_000;

/// The thrust the crossing arms use: the distance from where the thing
/// stands to the line, plus one fixed-point unit. `P_TryMove` only calls
/// `P_CrossSpecialLine` for a line whose side actually flips, so landing
/// exactly on the line would not be a crossing. The extra unit has to
/// stay well under the thing's own radius of one map unit, or its box
/// clears the line instead of straddling it.
const CROSSING: i64 = 8 * 65536 + 1;

/// `p_spec.c`: what the seeded lines are given. A WR teleport retrigger,
/// one of the specials a monster's own crossing can reach
/// `P_CrossSpecialLine`'s switch for and native does not dispatch. Every
/// line in the level is given this special rather than only the one the
/// crossing happens to cross, so a plat special would trip `EV_DoPlat`'s
/// own tag-0 walk into every untagged sector at once; a special native
/// runs sidesteps that rather than proving anything about it.
const MONSTER_UNHANDLED: i64 = 97;

/// One arm per seeded row: its name and where the copy of `BEFORE` lands.
/// The tics are far apart so the arms cannot read each other's rows.
const ARMS: [(&str, u32); 4] = [
    ("split", 200),
    ("clamp", 300),
    ("plain", 400),
    ("crossed", 500),
];

#[derive(Row, Deserialize)]
struct Moved {
    tic: u32,
    x: i32,
    y: i32,
    momx: i32,
    momy: i32,
    unresolved: u64,
}

/// `FixedMul` against `FRICTION`, which is what `P_XYMovement` leaves on a
/// thing standing on its floor with speed to spare.
fn slowed(mom: i64) -> i32 {
    ((mom * FRICTION) >> 16) as i32
}

/// Where `P_XYMovement`'s loop puts a thing, given a momentum nothing
/// blocks. The first part takes the half C division leaves and the second
/// takes the half the shift leaves, so a negative odd axis loses a unit
/// against the axis that triggered the split.
fn spent(mom: i64) -> i64 {
    let mom = mom.clamp(-MAXMOVE, MAXMOVE);
    mom / 2 + (mom >> 1)
}

/// The momentum a thing keeps once the move is done.
///
/// `P_XYMovement` reads both axes together: a thing stops dead only when
/// neither reaches `STOPSPEED`, so an axis under it still takes friction
/// while the other axis is carrying speed.
fn left(momx: i64, momy: i64) -> (i32, i32) {
    let momx = momx.clamp(-MAXMOVE, MAXMOVE);
    let momy = momy.clamp(-MAXMOVE, MAXMOVE);
    let slow = |mom: i64| mom > -STOPSPEED && mom < STOPSPEED;
    if slow(momx) && slow(momy) {
        (0, 0)
    } else {
        (slowed(momx), slowed(momy))
    }
}

/// Where an arm puts the thing, how it sends it, and whether the lines it
/// crosses carry a special.
fn arm(name: &str) -> (i64, i64, i64, i64, bool) {
    match name {
        "split" => (OPEN.0, OPEN.1, SPLIT_MOMX, SPLIT_MOMY, false),
        "clamp" => (OPEN.0, OPEN.1, 0, OVER_MOMY, false),
        "plain" => (BELOW_A_LINE.0, BELOW_A_LINE.1, 0, CROSSING, false),
        _ => (BELOW_A_LINE.0, BELOW_A_LINE.1, 0, CROSSING, true),
    }
}

#[tokio::test]
async fn a_thing_spends_the_momentum_the_engine_spends() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_thrust").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let walk: Vec<Input> = (1..=BEFORE).map(Input::demo).collect();
    support::resident::run(&fixture, &walk, false).await;

    let put = |column: &'static str, value: String| {
        (
            column,
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {SLOT}, {value}, v)), \
                 p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    };
    let mut statements: Vec<sql::Statement> = Vec::new();
    for (name, at) in ARMS {
        let (x, y, momx, momy, specials) = arm(name);
        let mut overrides = vec![
            put("m_x", x.to_string()),
            put("m_y", y.to_string()),
            put("m_radius", NARROW.to_string()),
            put("m_momx", momx.to_string()),
            put("m_momy", momy.to_string()),
        ];
        if specials {
            // Every line made to carry one, so what the move crosses is
            // whatever it crosses and the arm does not depend on the map
            // putting a special where the thrust happens to go.
            overrides.push((
                "line_special",
                format!("arrayMap(v -> toInt16({MONSTER_UNHANDLED}), p.line_special)"),
            ));
        }
        statements.extend(
            seed::row(&db, at, BEFORE, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        statements.extend(sim::tick::run_statement(
            &db,
            &[Input::keys(at + 1, 0, (0, 0))],
        ));
    }

    // Two things standing close enough to fail the general movers' own
    // reach test, thrust the same way along the axis neither stands apart
    // on, so their real destinations never come closer than they started.
    const CROWD_AT: u32 = 700;
    // `seed::row` takes the first override it finds for a column, so both
    // slots' own values for one column have to sit in the one expression.
    let put_both = |column: &'static str, one: String, two: String| {
        (
            column,
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {SLOT}, {one}, if(k = {SLOT2}, {two}, v))), \
                 p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    };
    let crowd_overrides = [
        put_both(
            "m_x",
            OPEN.0.to_string(),
            (OPEN.0 + CROWD_APART).to_string(),
        ),
        put_both("m_y", OPEN.1.to_string(), OPEN.1.to_string()),
        put_both("m_radius", NARROW.to_string(), NARROW.to_string()),
        put_both("m_momx", "0".to_owned(), "0".to_owned()),
        put_both("m_momy", CROWD_MOMY.to_string(), CROWD_MOMY.to_string()),
    ];
    statements.extend(
        seed::row(&db, CROWD_AT, BEFORE, &crowd_overrides)
            .into_iter()
            .map(sql::Statement::sql),
    );
    statements.extend(sim::tick::run_statement(
        &db,
        &[Input::keys(CROWD_AT + 1, 0, (0, 0))],
    ));

    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let wanted: Vec<String> = ARMS
        .iter()
        .flat_map(|(_, at)| [at.to_string(), (at + 1).to_string()])
        .collect();
    let rows: Vec<Moved> = fixture
        .rows(&format!(
            "SELECT tic, m_x[{SLOT}] AS x, m_y[{SLOT}] AS y, \
             m_momx[{SLOT}] AS momx, m_momy[{SLOT}] AS momy, unresolved \
             FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await;
    #[derive(Row, Deserialize)]
    struct Crowd {
        unresolved: u64,
        x: i32,
        y: i32,
        momx: i32,
        momy: i32,
        x2: i32,
        y2: i32,
        momx2: i32,
        momy2: i32,
    }
    let crowd: Crowd = fixture
        .rows(&format!(
            "SELECT unresolved, m_x[{SLOT}] AS x, m_y[{SLOT}] AS y, \
             m_momx[{SLOT}] AS momx, m_momy[{SLOT}] AS momy, \
             m_x[{SLOT2}] AS x2, m_y[{SLOT2}] AS y2, \
             m_momx[{SLOT2}] AS momx2, m_momy[{SLOT2}] AS momy2 \
             FROM {db}.native_state WHERE tic = {}",
            CROWD_AT + 1
        ))
        .await
        .into_iter()
        .next()
        .expect("the crowd arm's own tic ran");
    fixture.finish().await;
    assert_eq!(
        rows.len(),
        ARMS.len() * 2,
        "a seeded row and a tic from it for every arm"
    );
    let at = |tic: u32| {
        rows.iter()
            .find(|row| row.tic == tic)
            .unwrap_or_else(|| panic!("no row for tic {tic}"))
    };

    // The momentum over half of `MAXMOVE` is spent in two parts.
    let (before, after) = (at(200), at(201));
    assert_eq!(
        (before.momx as i64, before.momy as i64),
        (SPLIT_MOMX, SPLIT_MOMY),
        "the seeded row carries the momentum the arm asked for"
    );
    // The whole of the move has to land, or the wall and not the halving
    // is what this would be reading.
    assert_eq!(
        after.unresolved, 0,
        "the split arm runs, so nothing the move met stopped it"
    );
    assert_eq!(
        (after.x as i64, after.y as i64),
        (
            before.x as i64 + spent(SPLIT_MOMX),
            before.y as i64 + spent(SPLIT_MOMY)
        ),
        "both parts of the move land"
    );
    assert_eq!(
        (after.momx, after.momy),
        left(SPLIT_MOMX, SPLIT_MOMY),
        "friction takes what the move left"
    );
    // The halving reaches both axes once either one triggers it, and the
    // two halves round in opposite directions, so an odd axis lands
    // differently depending on its sign. Reading the same value for both
    // would mean the split never happened.
    assert_ne!(
        spent(SPLIT_MOMY),
        SPLIT_MOMY,
        "the odd axis the split rides on loses a unit"
    );
    assert_eq!(
        spent(SPLIT_MOMX),
        SPLIT_MOMX,
        "the odd negative axis lands whole, because the first half \
         truncates towards zero where the second floors"
    );

    // The momentum over `MAXMOVE` is cut down to it.
    let (before, after) = (at(300), at(301));
    assert_eq!(
        before.momy as i64, OVER_MOMY,
        "the seeded row carries more than the clamp allows"
    );
    assert_eq!(
        after.unresolved, 0,
        "the clamp arm runs, so nothing the move met stopped it"
    );
    assert_eq!(
        after.y as i64 - before.y as i64,
        spent(MAXMOVE),
        "the move is the clamp and not what was asked for"
    );
    assert_eq!(
        (after.momx, after.momy),
        left(0, OVER_MOMY),
        "friction reads the clamp too"
    );

    // `P_CrossSpecialLine` is what a move that landed owes the special
    // lines it crossed, and a thrust can push a monster's own move across
    // one. `MONSTER_UNHANDLED` is a special a monster's own crossing
    // reaches the switch for and this engine does not dispatch, so the
    // crossing leaves the tic unresolved rather than running or dropping
    // it silently. Every line is given the special rather than depending
    // on the map putting one where the thrust happens to go.
    //
    // The two arms are the test. They are the same thrust from the same
    // place and differ only in whether the lines carry a special, so a tic
    // unresolved for any other reason would leave both of them unresolved.
    let (seeded, plain, crossed) = (at(400), at(401), at(501));
    assert_eq!(
        plain.unresolved, 0,
        "the same thrust over lines with no special runs"
    );
    assert_ne!(
        crossed.y, seeded.y,
        "the thrust moved it, so the move landed"
    );
    assert_eq!(crossed.y, plain.y, "and moved it to the same place");
    assert_eq!(
        crossed.unresolved,
        sim::unresolved::TX_CROSSED,
        "the special line the move crossed is not run, so the tic says so"
    );

    // Two things thrust the same way, close enough that the general
    // movers' own reach test alone would call them crowded, resolve: the
    // consulted set the fold builds sees neither destination came closer
    // to the other than it started.
    assert_eq!(
        crowd.unresolved, 0,
        "two things thrust apart resolve, not just crowded"
    );
    assert_eq!(
        (crowd.y as i64, crowd.momy as i64),
        (OPEN.1 + spent(CROWD_MOMY), left(0, CROWD_MOMY).1 as i64),
        "the first thing's own move lands"
    );
    assert_eq!(
        (crowd.y2 as i64, crowd.momy2 as i64),
        (OPEN.1 + spent(CROWD_MOMY), left(0, CROWD_MOMY).1 as i64),
        "and so does the second thing's own move, the same way"
    );
    assert_eq!(
        crowd.x2 - crowd.x,
        CROWD_APART as i32,
        "moving the same way leaves the gap between them exactly as it was"
    );
    assert_eq!(crowd.momx, 0, "no momentum on the axis they stand apart on");
    assert_eq!(crowd.momx2, 0, "for the second thing either");
}
