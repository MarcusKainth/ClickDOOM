//! `P_DamageMobj`'s player branch, against a real ClickHouse server.
//!
//! A claw is seeded the way `native/tests/sim_troop_live.rs` seeds one,
//! aimed at the player instead of a second imp: an imp beside the player,
//! one tic from `A_TroopAttack`'s own frame.
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

/// The imp that runs the routine. `MT_TROOP`, on the level's own list, and
/// stands still at gametic 40.
const ATTACKER: usize = 116;

/// `states.tsv`: the frame carrying `A_FaceTarget`, and the frame after it
/// carrying `A_TroopAttack`. Seeding the first with one tic of wait left
/// puts the routine on the tic the arm runs.
const FACE: i32 = 453;
const ATTACK: i32 = 454;

/// How far from the player the imp stands. `MELEERANGE` is sixty four
/// units and `P_CheckMeleeRange` measures against that less twenty, plus
/// the player's own radius; four units is inside its reach.
const NEAR: i64 = 4 * 65536;

/// `doomdef.h`
const ARMOR_NONE: i64 = 0;
const ARMOR_GREEN: i64 = 1;
const ARMOR_BLUE: i64 = 2;
/// Comfortably more than a claw's own worst case (24) can spend, so the
/// armour arms never run out of what they save.
const STARTING_ARMORPOINTS: i64 = 100;

/// One arm per seeded row: its name, where the copy of `BEFORE` lands and
/// the armour it starts the player with.
const ARMS: [(&str, u32, i64); 3] = [
    ("no_armor", 200, ARMOR_NONE),
    ("green_armor", 300, ARMOR_GREEN),
    ("blue_armor", 400, ARMOR_BLUE),
];

#[derive(Row, Deserialize)]
struct Hit {
    tic: u32,
    p_health: i32,
    p_armorpoints: i32,
    p_armortype: i32,
    p_damagecount: i32,
    p_attacker: u32,
    player_health: i32,
    attacker_state: i32,
    unresolved: u64,
}

/// A column of one slot replaced, leaving every other slot alone.
fn put(column: &'static str, slot: usize, value: String, cast: &str) -> (&'static str, String) {
    (
        column,
        format!(
            "arrayMap((v, k) -> {cast}(if(k = {slot}, {value}, v)), \
             p.{column}, arrayEnumerate(p.{column}))"
        ),
    )
}

#[tokio::test]
async fn a_claw_reaches_the_player_through_its_own_armour() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_player_damage").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let mut statements: Vec<sql::Statement> = Vec::new();
    for (_, at, armortype) in ARMS {
        let overrides = [
            // The imp beside the player, in the frame whose next carries
            // the routine, with one tic of wait left on it.
            put(
                "m_x",
                ATTACKER,
                format!("p.m_x[p.p_mo] + toInt32({NEAR})"),
                "toInt32",
            ),
            put("m_y", ATTACKER, "p.m_y[p.p_mo]".to_owned(), "toInt32"),
            put("m_z", ATTACKER, "p.m_z[p.p_mo]".to_owned(), "toInt32"),
            put("m_state", ATTACKER, FACE.to_string(), "toInt32"),
            put("m_tics", ATTACKER, "1".to_owned(), "toInt32"),
            put("m_target", ATTACKER, "p.p_mo".to_owned(), "toUInt32"),
            ("p_health", "toInt32(100)".to_owned()),
            ("p_armortype", format!("toInt32({armortype})")),
            (
                "p_armorpoints",
                format!(
                    "toInt32({})",
                    if armortype == ARMOR_NONE {
                        0
                    } else {
                        STARTING_ARMORPOINTS
                    }
                ),
            ),
            ("p_damagecount", "toInt32(0)".to_owned()),
            ("p_attacker", "toUInt32(0)".to_owned()),
        ];
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

    let wanted: Vec<String> = ARMS
        .iter()
        .flat_map(|(_, at, _)| [at.to_string(), (at + 1).to_string()])
        .collect();
    let rows: Vec<Hit> = fixture
        .rows(&format!(
            "SELECT tic, p_health, p_armorpoints, p_armortype, p_damagecount, p_attacker, \
             m_health[p_mo] AS player_health, m_state[{ATTACKER}] AS attacker_state, \
             unresolved \
             FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await;
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

    // The bare arm: no armour, so the raw roll reaches health outright.
    // `A_TroopAttack`'s own damage is `(P_Random()%8+1)*3`, and every arm
    // seeds the same imp at the same state with the same starting
    // `prndindex`, so the roll is identical across all three.
    let (before, after) = (at(200), at(201));
    assert_eq!(
        before.attacker_state, FACE,
        "the seeded row is a tic from the routine"
    );
    assert_eq!(after.attacker_state, ATTACK, "and the cycle reaches it");
    assert_eq!(
        after.unresolved, 0,
        "a living player's own hit resolves, unlike a monster's"
    );
    let raw_damage = before.p_health - after.p_health;
    assert!(
        (3..=24).contains(&raw_damage) && raw_damage % 3 == 0,
        "the claw's own damage is three times one to eight: {raw_damage}"
    );
    assert_eq!(
        before.player_health - after.player_health,
        raw_damage,
        "the mobj's own health shares the same number"
    );
    assert_eq!(after.p_armorpoints, 0, "no armour, nothing saved");
    assert_eq!(after.p_armortype, 0);
    assert_eq!(
        after.p_damagecount, raw_damage,
        "the tint takes the whole hit"
    );
    assert_eq!(
        after.p_attacker, ATTACKER as u32,
        "the player's own attacker is the imp that clawed it"
    );

    // The armoured arms save a third or a half of the same roll.
    for (name, divisor) in [("green_armor", 3), ("blue_armor", 2)] {
        let (_, at_tic, _) = ARMS.iter().find(|(n, _, _)| *n == name).unwrap();
        let (before, after) = (at(*at_tic), at(*at_tic + 1));
        assert_eq!(after.unresolved, 0, "{name}");
        let saved = raw_damage / divisor;
        assert_eq!(
            before.p_armorpoints - after.p_armorpoints,
            saved,
            "{name}: the armour absorbs its own share"
        );
        assert_eq!(
            before.p_health - after.p_health,
            raw_damage - saved,
            "{name}: health takes what the armour did not"
        );
        assert_eq!(
            before.player_health - after.player_health,
            raw_damage - saved,
            "{name}: the mobj's own health shares the same number"
        );
        assert_eq!(
            after.p_damagecount,
            raw_damage - saved,
            "{name}: the tint takes the post-armour damage"
        );
    }
}
