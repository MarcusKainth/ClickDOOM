//! A pointer between thinkers when the list compacts, against a real
//! ClickHouse server.
//!
//! `P_TouchSpecialThing` takes a thing off the list and the mobj arrays
//! close the gap, so every slot above it moves down by one. A pointer
//! holds a slot, so it has to move down with the thing it names and go to
//! 0 where that thing was the one taken. `demo3` reaches no pointer that
//! names a slot above a pickup, so the pointers are seeded into a state
//! row and the pickup is one the demo makes.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::seed;

/// The last tic run before the seeded row, and how far the run goes after
/// it. The seeded row stands where the tic before it left the player, so
/// the walk is one tic behind the demo and reaches the thing it picks up
/// one tic later than the demo does.
const BEFORE: u32 = 45;
const SEED_TIC: u32 = 46;
const LAST: u32 = 50;

#[derive(Row, Deserialize)]
struct Compacted {
    tic: u32,
    sprite: Vec<i32>,
    target: Vec<u32>,
    tracer: Vec<u32>,
    soundtarget: Vec<u32>,
    attacker: u32,
    unresolved: u8,
}

#[tokio::test]
async fn a_pointer_follows_the_thing_it_names_through_a_pickup() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_compact").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    // Every thing points at itself and at the one after it, so whichever
    // thing the pickup takes, one pointer names it and the ones above it
    // move. The two sectors given a sound target are the last two,
    // because `A_Look` says the tic could not be produced on a sector
    // that has heard something and nothing looks out of those.
    let seeded: Vec<sql::Statement> = seed::row(
        &db,
        SEED_TIC,
        BEFORE,
        &[
            (
                "m_target",
                "arrayMap(k -> toUInt32(k), arrayEnumerate(p.m_x))".to_owned(),
            ),
            (
                "m_tracer",
                "arrayMap(k -> toUInt32(if(k < length(p.m_x), k + 1, 0)), \
                 arrayEnumerate(p.m_x))"
                    .to_owned(),
            ),
            (
                "sec_soundtarget",
                "arrayMap(s -> toUInt32(multiIf(\
                 s = length(p.sec_soundtarget), length(p.m_x), \
                 s = length(p.sec_soundtarget) - 1, 1, 0)), \
                 arrayEnumerate(p.sec_soundtarget))"
                    .to_owned(),
            ),
            ("p_attacker", "toUInt32(length(p.m_x))".to_owned()),
        ],
    )
    .into_iter()
    .map(sql::Statement::sql)
    .collect();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let run = sim::tick::demo_statement(&db, SEED_TIC + 1, LAST);
    if let Err(error) = fixture.execute(&[run]).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Compacted> = fixture
        .rows(&format!(
            "SELECT tic, m_sprite AS sprite, m_target AS target, m_tracer AS tracer, \
             sec_soundtarget AS soundtarget, p_attacker AS attacker, unresolved \
             FROM {db}.native_state WHERE tic >= {SEED_TIC} ORDER BY tic"
        ))
        .await;
    fixture.finish().await;

    let before = rows.first().expect("the seeded row");
    assert_eq!(before.tic, SEED_TIC);
    for row in &rows {
        assert_eq!(row.unresolved, 0, "gametic {} was carried through", row.tic);
    }
    let after = rows
        .iter()
        .find(|row| row.sprite.len() < before.sprite.len())
        .expect("the walk reaches something to pick up");
    let slots = before.sprite.len();
    assert_eq!(
        after.sprite.len(),
        slots - 1,
        "the pickup takes one thing off the list"
    );

    // Which slot went, read off the arrays either side of the pickup.
    let taken = before
        .sprite
        .iter()
        .zip(&after.sprite)
        .position(|(before, after)| before != after)
        .map_or(slots, |at| at + 1);
    let moved_to = |slot: usize| -> u32 {
        match slot {
            slot if slot == taken => 0,
            slot if slot < taken => slot as u32,
            slot => slot as u32 - 1,
        }
    };
    let surviving = || (1..=slots).filter(|slot| *slot != taken);

    // A pointer at a thing that survives names the slot it moved to, and
    // one at the thing that was taken is none.
    assert_eq!(
        after.target,
        surviving().map(moved_to).collect::<Vec<u32>>(),
        "every thing still points at itself, slot {taken} of {slots} taken"
    );
    assert_eq!(
        after.tracer,
        surviving()
            .map(|slot| if slot == slots { 0 } else { moved_to(slot + 1) })
            .collect::<Vec<u32>>(),
        "every thing points at the one after it, slot {taken} of {slots} taken"
    );
    assert_eq!(
        after.attacker,
        moved_to(slots),
        "the player's attacker was the last thing on the list"
    );
    let sectors = after.soundtarget.len();
    assert_eq!(
        after.soundtarget[sectors - 1],
        moved_to(slots),
        "a sector that heard the last thing on the list follows it down"
    );
    assert_eq!(
        after.soundtarget[sectors - 2],
        1,
        "a sector that heard the player still hears the player"
    );
}
