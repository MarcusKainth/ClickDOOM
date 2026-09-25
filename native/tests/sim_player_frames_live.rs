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

use support::arms::{Arm, ArmChecks, Arms};
use support::db::Fixture;
use support::resident::Session;

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

/// The player's mobj seeded into `state` with one tic of wait left, at
/// `at`, and walked `tics` tics from it.
///
/// The player stands still, so nothing it does moves it out of the frame
/// the seed put it in. The picture goes in with the state, because
/// `P_SetMobjState` writes both and a row carrying one without the other
/// is a row the engine could not have produced.
fn frames(name: &'static str, at: u32, state: i32, tics: u32) -> Arm {
    let (sprite, frame) = support::damage::picture(i64::from(state));
    Arm {
        name,
        from: BEFORE,
        overrides: vec![
            put("m_state", state.to_string(), "toInt32"),
            put("m_tics", "1".to_owned(), "toInt32"),
            put("m_sprite", sprite.to_string(), "toInt32"),
            put("m_frame", frame.to_string(), "toInt32"),
            put("m_momx", "0".to_owned(), "toInt32"),
            put("m_momy", "0".to_owned(), "toInt32"),
        ],
        at,
        inputs: (1..=tics)
            .map(|step| Input::keys(at + step, 0, (0, 0)))
            .collect(),
    }
}

/// Both chains, seeded from one walk to [`BEFORE`] and driven through one
/// session.
#[tokio::test]
async fn the_player_walks_its_pain_and_death_frames() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_player_frames").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let arms = Arms::new(
        (1..=BEFORE).map(Input::demo).collect(),
        vec![
            frames("pain", PAIN_AT, S_PLAY_PAIN, PAIN_TICS),
            frames("death", DEATH_AT, S_PLAY_DIE1, DEATH_TICS),
        ],
    );
    let mut session = Session::open(&fixture, false).await;
    arms.drive(&fixture, &mut session).await;
    session.close().await;

    let mut checks = arms.checks();
    for arm in arms.all() {
        let rows: Vec<Frame> = arm
            .rows(
                &fixture,
                "native_state",
                "tic, m_state[p_mo] AS state, m_tics[p_mo] AS tics, \
                 m_sprite[p_mo] AS sprite, m_frame[p_mo] AS frame, \
                 m_flags[p_mo] AS flags, unresolved",
            )
            .await;
        let mut c = checks.arm(arm.name);
        c.eq(
            rows.len(),
            arm.inputs.len() + 1,
            "the seeded row and every tic walked from it",
        );
        match arm.name {
            "pain" => the_player_walks_its_pain_frames_back_to_standing(c, &rows),
            _ => the_player_walks_its_death_frames_to_a_corpse(c, &rows),
        }
    }
    fixture.finish().await;
    checks.finish();
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
fn the_player_walks_its_pain_frames_back_to_standing(mut c: ArmChecks<'_>, rows: &[Frame]) {
    for row in rows {
        c.eq(
            row.unresolved,
            0,
            format!("tic {} carries a routine this does not run", row.tic),
        );
        c.eq(
            (row.sprite, row.frame),
            {
                let (sprite, frame) = support::damage::picture(i64::from(row.state));
                (sprite as i32, frame as i32)
            },
            format!("tic {}: the picture follows the frame", row.tic),
        );
        c.eq(
            row.flags & MF_SOLID,
            MF_SOLID,
            format!(
                "tic {}: nothing in the pain chain takes MF_SOLID off",
                row.tic
            ),
        );
    }
    let states: Vec<i32> = entered(rows).into_iter().map(|(_, state)| state).collect();
    c.eq(
        states,
        vec![S_PLAY_PAIN, S_PLAY_PAIN2, S_PLAY],
        "the chain runs pain, pain two, standing",
    );
    let Some(last) = rows.last() else {
        c.check(false, "the walk wrote rows");
        return;
    };
    c.eq(last.state, S_PLAY, "the last frame");
    c.eq(last.tics, -1, "standing waits forever");
}

/// `A_PlayerScream` makes a noise, `A_Fall` takes `MF_SOLID` off, and the
/// chain stops on a frame that waits forever.
fn the_player_walks_its_death_frames_to_a_corpse(mut c: ArmChecks<'_>, rows: &[Frame]) {
    for row in rows {
        c.eq(
            row.unresolved,
            0,
            format!("tic {} carries a routine this does not run", row.tic),
        );
        c.eq(
            (row.sprite, row.frame),
            {
                let (sprite, frame) = support::damage::picture(i64::from(row.state));
                (sprite as i32, frame as i32)
            },
            format!("tic {}: the picture follows the frame", row.tic),
        );
    }
    let entered = entered(rows);
    let states: Vec<i32> = entered.iter().map(|(_, state)| *state).collect();
    c.eq(
        states,
        (S_PLAY_DIE1..=S_PLAY_DIE7).collect::<Vec<i32>>(),
        "the chain runs every death frame in order",
    );
    // `A_Fall` runs on the way into S_PLAY_DIE3 and not before it.
    let Some(fell) = entered
        .iter()
        .find(|(_, state)| *state == S_PLAY_DIE3)
        .map(|(tic, _)| *tic)
    else {
        c.check(false, "the chain reaches the frame A_Fall sits on");
        return;
    };
    for row in rows {
        let solid = row.flags & MF_SOLID;
        match row.tic < fell {
            true => c.eq(solid, MF_SOLID, format!("tic {}: still solid", row.tic)),
            false => c.eq(
                solid,
                0,
                format!("tic {}: A_Fall has taken MF_SOLID off", row.tic),
            ),
        }
    }
    let Some(last) = rows.last() else {
        c.check(false, "the walk wrote rows");
        return;
    };
    c.eq(last.state, S_PLAY_DIE7, "the last frame");
    c.eq(last.tics, -1, "the last death frame waits forever");
}
