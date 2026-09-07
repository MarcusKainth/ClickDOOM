//! Where the sector thinkers run in the thinker list, against a real
//! ClickHouse server.
//!
//! `P_SpawnSpecials` adds them after `P_LoadThings`, so they draw after
//! the things the level setup spawned and before anything spawned during
//! play. `demo3` reaches that at gametic 206, where an imp's fireball is
//! one tic from the player: a light flash draws between the last monster
//! that chases and the fireball's own impact. The engine's rows either
//! side of that tic are the fixture, and the tic is run from the first of
//! them.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use std::collections::HashMap;
use std::path::Path;

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;

/// The tic the fixture's first row leaves behind, and the one run from it.
const SEED_TIC: u32 = 205;
const RUN_TIC: u32 = 206;

/// The fireball's slot at both tics. It was thrown during play, so it
/// stands above the boundary and the light flash draws ahead of it.
const FIREBALL: usize = 265;

/// The engine's own rows at gametics 205 and 206, by gametic and column.
///
/// The header names the contract's columns in order, so a fixture written
/// against an older field list fails here rather than lining the values up
/// against the wrong names.
fn engine() -> HashMap<u32, HashMap<String, String>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("demo3-thinker-order.tsv");
    let text = std::fs::read_to_string(path).expect("the fixture is committed");
    let columns: Vec<&str> = text
        .lines()
        .find_map(|line| line.strip_prefix("# columns\t"))
        .expect("the fixture names its columns")
        .split('\t')
        .collect();
    assert_eq!(
        &columns[3..],
        clickdoom_spec::native_state::all_fields().as_slice(),
        "the fixture and the contract disagree on the column list"
    );
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let values: Vec<&str> = line.split('\t').collect();
            let gametic: u32 = values[1].parse().expect("a gametic");
            let row = columns
                .iter()
                .zip(&values)
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect();
            (gametic, row)
        })
        .collect()
}

/// The fixture's rows without its comment header, which is what the probe
/// loader takes.
fn seed_row(gametic: u32) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("demo3-thinker-order.tsv");
    let text = std::fs::read_to_string(path).expect("the fixture is committed");
    let want = gametic.to_string();
    let mut out = String::new();
    for line in text.lines() {
        if !line.starts_with('#') && line.split('\t').nth(1) == Some(want.as_str()) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.into_bytes()
}

#[derive(Row, Deserialize)]
struct Ran {
    setup_things: u32,
    s_count: Vec<i32>,
    state: i32,
    tics: i32,
    flags: i32,
    unresolved: u64,
}

#[tokio::test]
async fn a_light_flash_draws_between_a_map_thing_and_a_thrown_one() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_thinker_order").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    support::probe::load(&fixture, &seed_row(SEED_TIC)).await;
    let run = sim::tick::demo_statement(&db, RUN_TIC, RUN_TIC);
    if let Err(error) = fixture.execute(&[run]).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let ran: Ran = fixture
        .scalar(&format!(
            "SELECT setup_things, s_count, m_state[{FIREBALL}] AS state, \
             m_tics[{FIREBALL}] AS tics, m_flags[{FIREBALL}] AS flags, unresolved \
             FROM {db}.native_state WHERE tic = {RUN_TIC}"
        ))
        .await;
    fixture.finish().await;

    let engine = engine();
    let theirs = |column: &str| engine[&RUN_TIC][column].clone();
    let array = |column: &str| -> Vec<i32> {
        theirs(column)
            .trim_matches(['[', ']'])
            .split(',')
            .map(|v| v.parse().expect("an array of counts"))
            .collect()
    };
    let at = |column: &str| -> i32 { array(column)[FIREBALL - 1] };

    // Nothing comes off the list this tic, so the boundary the seeded
    // row carries stands.
    assert_eq!(
        ran.setup_things,
        theirs("setup_things").parse::<u32>().unwrap()
    );

    // The flash's own count is `(P_Random() & mintime) + 1`, so it reads
    // the table where the draws of the slots at or below the boundary
    // leave it and nowhere else.
    assert_eq!(ran.s_count, array("s_count"), "the sector thinkers' counts");

    // `P_ExplodeMissile` shortens the death frame by `P_Random() & 3`,
    // drawn after the impact's own and the damage call's, all of which
    // stand behind the flash.
    assert_eq!(ran.state, at("m_state"), "the fireball's frame");
    assert_eq!(ran.tics, at("m_tics"), "the wait its death frame took");
    assert_eq!(ran.flags, at("m_flags"), "MF_MISSILE is off");

    // The hit lands on the player, a path the missile thinker does not
    // resolve.
    assert_eq!(ran.unresolved, sim::unresolved::MISSILE_STUCK);
}
