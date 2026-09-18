//! The player's own state cycle through its pain and death frames,
//! against a real ClickHouse server.
//!
//! `P_MobjThinker` runs the player's mobj like any other thinker, and
//! `player.rs` runs that cycle rather than `mobj.rs`, so the routines
//! those frames carry are exempted there. `A_Pain`, `A_PlayerScream` and
//! `A_XScream` make a noise and leave nothing behind; `A_Fall` takes
//! `MF_SOLID` off.
//!
//! Both arms seed a frame and walk the chain out through one resident
//! session, so the tic statement's analysis is paid once rather than once
//! per tic.
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

/// The tic the arm copies its row from. Gametic 40 is early enough that no
/// monster has woken and the list still holds the level's own things.
const BEFORE: u32 = 40;

/// `states.tsv`: the player's own pain chain and the frame it returns to.
/// `S_PLAY_PAIN2` carries `A_Pain`.
const S_PLAY: i32 = 149;
const S_PLAY_PAIN: i32 = 156;
const S_PLAY_PAIN2: i32 = 157;

/// `states.tsv`: the player's own death chain. `S_PLAY_DIE2` carries
/// `A_PlayerScream`, `S_PLAY_DIE3` carries `A_Fall`, and `S_PLAY_DIE7`
/// waits forever.
const S_PLAY_DIE1: i32 = 158;
const S_PLAY_DIE3: i32 = 160;
const S_PLAY_DIE7: i32 = 164;

/// `p_mobj.h`
const MF_SOLID: i32 = 2;

/// Where each arm's seeded row lands, and how many tics it walks from it.
/// The pain chain waits four tics a frame and the death chain ten, so the
/// walks are sized to reach the end of each.
const PAIN_AT: u32 = 100;
const PAIN_TICS: u32 = 12;
const DEATH_AT: u32 = 200;
const DEATH_TICS: u32 = 70;

#[derive(Row, Deserialize)]
struct Frame {
    tic: u32,
    state: i32,
    tics: i32,
    sprite: i32,
    frame: i32,
    flags: i32,
    unresolved: u64,
}

/// A column of the player's own mobj slot replaced, leaving every other
/// slot alone.
fn put(column: &'static str, value: String, cast: &str) -> (&'static str, String) {
    (
        column,
        format!(
            "arrayMap((v, k) -> {cast}(if(k = p.p_mo, {value}, v)), \
             p.{column}, arrayEnumerate(p.{column}))"
        ),
    )
}

/// Seeds the player's mobj into `state` with one tic of wait left, walks
/// `tics` tics from it and reads every row back.
async fn walk(suffix: &str, at: u32, state: i32, tics: u32) -> Vec<Frame> {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create(&format!("sim_player_frames_{suffix}")).await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let start: Vec<Input> = (1..=BEFORE).map(Input::demo).collect();
    support::resident::run(&fixture, &start, false).await;

    // The player stands still, so nothing it does moves it out of the
    // frame the seed put it in. The picture goes in with the state,
    // because `P_SetMobjState` writes both and a row carrying one without
    // the other is a row the engine could not have produced.
    let (sprite, frame) = support::damage::picture(i64::from(state));
    let overrides = [
        put("m_state", state.to_string(), "toInt32"),
        put("m_tics", "1".to_owned(), "toInt32"),
        put("m_sprite", sprite.to_string(), "toInt32"),
        put("m_frame", frame.to_string(), "toInt32"),
        put("m_momx", "0".to_owned(), "toInt32"),
        put("m_momy", "0".to_owned(), "toInt32"),
    ];
    let seeded: Vec<sql::Statement> = seed::row(&db, at, BEFORE, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let steps: Vec<Input> = (1..=tics)
        .map(|step| Input::keys(at + step, 0, (0, 0)))
        .collect();
    support::resident::run(&fixture, &steps, false).await;

    let rows: Vec<Frame> = fixture
        .rows(&format!(
            "SELECT tic, m_state[p_mo] AS state, m_tics[p_mo] AS tics, \
             m_sprite[p_mo] AS sprite, m_frame[p_mo] AS frame, \
             m_flags[p_mo] AS flags, unresolved \
             FROM {db}.native_state WHERE tic BETWEEN {at} AND {} ORDER BY tic",
            at + tics
        ))
        .await;
    fixture.finish().await;
    assert_eq!(
        rows.len(),
        tics as usize + 1,
        "the seeded row and every tic walked from it"
    );
    rows
}

/// Every frame the walk stood in, in order, with the tic it first showed.
fn entered(rows: &[Frame]) -> Vec<(u32, i32)> {
    let mut seen: Vec<(u32, i32)> = Vec::new();
    for row in rows {
        if seen.last().map(|(_, state)| *state) != Some(row.state) {
            seen.push((row.tic, row.state));
        }
    }
    seen
}

/// `A_Pain` makes a noise and nothing else, so the pain chain runs through
/// and back into the frame the player stands in.
#[tokio::test]
async fn the_player_walks_its_pain_frames_back_to_standing() {
    let rows = walk("pain", PAIN_AT, S_PLAY_PAIN, PAIN_TICS).await;
    for row in &rows {
        assert_eq!(
            row.unresolved, 0,
            "tic {} carries a routine this does not run",
            row.tic
        );
        assert_eq!(
            (row.sprite, row.frame),
            {
                let (sprite, frame) = support::damage::picture(i64::from(row.state));
                (sprite as i32, frame as i32)
            },
            "tic {}: the picture follows the frame",
            row.tic
        );
        assert_eq!(
            row.flags & MF_SOLID,
            MF_SOLID,
            "tic {}: nothing in the pain chain takes MF_SOLID off",
            row.tic
        );
    }
    let states: Vec<i32> = entered(&rows).into_iter().map(|(_, state)| state).collect();
    assert_eq!(
        states,
        vec![S_PLAY_PAIN, S_PLAY_PAIN2, S_PLAY],
        "the chain runs pain, pain two, standing"
    );
    let last = rows.last().expect("the walk wrote rows");
    assert_eq!(last.state, S_PLAY);
    assert_eq!(last.tics, -1, "standing waits forever");
}

/// `A_PlayerScream` makes a noise, `A_Fall` takes `MF_SOLID` off, and the
/// chain stops on a frame that waits forever.
#[tokio::test]
async fn the_player_walks_its_death_frames_to_a_corpse() {
    let rows = walk("death", DEATH_AT, S_PLAY_DIE1, DEATH_TICS).await;
    for row in &rows {
        assert_eq!(
            row.unresolved, 0,
            "tic {} carries a routine this does not run",
            row.tic
        );
        assert_eq!(
            (row.sprite, row.frame),
            {
                let (sprite, frame) = support::damage::picture(i64::from(row.state));
                (sprite as i32, frame as i32)
            },
            "tic {}: the picture follows the frame",
            row.tic
        );
    }
    let entered = entered(&rows);
    let states: Vec<i32> = entered.iter().map(|(_, state)| *state).collect();
    assert_eq!(
        states,
        (S_PLAY_DIE1..=S_PLAY_DIE7).collect::<Vec<i32>>(),
        "the chain runs every death frame in order"
    );
    // `A_Fall` runs on the way into S_PLAY_DIE3 and not before it.
    let fell = entered
        .iter()
        .find(|(_, state)| *state == S_PLAY_DIE3)
        .map(|(tic, _)| *tic)
        .expect("the chain reaches the frame A_Fall sits on");
    for row in &rows {
        let solid = row.flags & MF_SOLID;
        match row.tic < fell {
            true => assert_eq!(solid, MF_SOLID, "tic {}: still solid", row.tic),
            false => assert_eq!(solid, 0, "tic {}: A_Fall has taken MF_SOLID off", row.tic),
        }
    }
    let last = rows.last().expect("the walk wrote rows");
    assert_eq!(last.state, S_PLAY_DIE7);
    assert_eq!(last.tics, -1, "the last death frame waits forever");
}
