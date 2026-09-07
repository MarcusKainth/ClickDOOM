//! Two in-flight missiles landing on the same target in one tic, against a
//! real ClickHouse server.
//!
//! Follows `sim_missile_kill_drop_live.rs`'s own seeding shape: a target
//! turned into a zombieman, two things turned into fireballs, each aimed to
//! land on it in the same tic, on parallel lines far enough apart that
//! their own radii never touch in flight. The target's own health starts
//! high enough that neither roll nor both together kill it, so the second
//! missile's own call reads what the first left rather than the tic-start
//! arrays.
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

const BEFORE: u32 = 40;
const MISSILE_A: usize = 116;
const MISSILE_B: usize = 118;
const TARGET: usize = 117;

const MT_TROOPSHOT: i32 = 31;
const MT_POSSESSED: i32 = 1;

const TROOPSHOT_FLAGS: i64 = 67_088;
const POSSESSED_FLAGS: i64 = 4_194_310;
const TROOPSHOT_RADIUS: i64 = 393_216;
const TROOPSHOT_HEIGHT: i64 = 524_288;
const POSSESSED_RADIUS: i64 = 1_310_720;
const TROOPSHOT_SPAWNSTATE: i32 = 97;
const TROOPSHOT_SPAWNTICS: i32 = 4;

const STEP: i64 = 5 * 65536;
const LANE: i64 = 900_000;
const TARGET_HEALTH: i32 = 1000;

fn put(column: &'static str, slot: usize, value: String, cast: &str) -> (&'static str, String) {
    (
        column,
        format!(
            "arrayMap((v, k) -> {cast}(if(k = {slot}, {value}, v)), \
             p.{column}, arrayEnumerate(p.{column}))"
        ),
    )
}

fn put_many(column: &'static str, slots: &[(usize, String)], cast: &str) -> (&'static str, String) {
    let arms: String = slots
        .iter()
        .map(|(slot, value)| format!("k = {slot}, {value}, "))
        .collect();
    (
        column,
        format!(
            "arrayMap((v, k) -> {cast}(multiIf({arms}v)), \
             p.{column}, arrayEnumerate(p.{column}))"
        ),
    )
}

#[derive(Row, Deserialize)]
struct Hit {
    tic: u32,
    target_health: i32,
    unresolved: u64,
    missile_a_state: i32,
    missile_b_state: i32,
    prndindex: u8,
}

#[tokio::test]
async fn two_fireballs_in_one_list_thread_a_zombieman_through_both() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_missile_same_target").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let at = BEFORE + 100;
    let overrides = [
        put_many(
            "m_type",
            &[
                (TARGET, MT_POSSESSED.to_string()),
                (MISSILE_A, MT_TROOPSHOT.to_string()),
                (MISSILE_B, MT_TROOPSHOT.to_string()),
            ],
            "toInt32",
        ),
        put_many(
            "m_flags",
            &[
                (TARGET, POSSESSED_FLAGS.to_string()),
                (MISSILE_A, TROOPSHOT_FLAGS.to_string()),
                (MISSILE_B, TROOPSHOT_FLAGS.to_string()),
            ],
            "toInt32",
        ),
        put_many(
            "m_threshold",
            &[
                (TARGET, "0".to_owned()),
                (MISSILE_A, "0".to_owned()),
                (MISSILE_B, "0".to_owned()),
            ],
            "toInt32",
        ),
        put("m_health", TARGET, TARGET_HEALTH.to_string(), "toInt32"),
        put_many(
            "m_x",
            &[
                (MISSILE_A, format!("p.m_x[{TARGET}] - {STEP}")),
                (MISSILE_B, format!("p.m_x[{TARGET}] - {STEP}")),
            ],
            "toInt32",
        ),
        put_many(
            "m_y",
            &[
                (MISSILE_A, format!("p.m_y[{TARGET}]")),
                (MISSILE_B, format!("p.m_y[{TARGET}] + {LANE}")),
            ],
            "toInt32",
        ),
        put_many(
            "m_z",
            &[
                (TARGET, format!("p.m_z[{TARGET}]")),
                (MISSILE_A, format!("p.m_z[{TARGET}]")),
                (MISSILE_B, format!("p.m_z[{TARGET}]")),
            ],
            "toInt32",
        ),
        put_many(
            "m_floorz",
            &[
                (MISSILE_A, format!("p.m_floorz[{TARGET}]")),
                (MISSILE_B, format!("p.m_floorz[{TARGET}]")),
            ],
            "toInt32",
        ),
        put_many(
            "m_ceilingz",
            &[
                (MISSILE_A, format!("p.m_ceilingz[{TARGET}]")),
                (MISSILE_B, format!("p.m_ceilingz[{TARGET}]")),
            ],
            "toInt32",
        ),
        put_many(
            "m_subsector",
            &[
                (MISSILE_A, format!("p.m_subsector[{TARGET}]")),
                (MISSILE_B, format!("p.m_subsector[{TARGET}]")),
            ],
            "toInt32",
        ),
        put_many(
            "m_momx",
            &[(MISSILE_A, STEP.to_string()), (MISSILE_B, STEP.to_string())],
            "toInt32",
        ),
        put_many(
            "m_momy",
            &[(MISSILE_A, "0".to_owned()), (MISSILE_B, "0".to_owned())],
            "toInt32",
        ),
        put_many(
            "m_momz",
            &[
                (TARGET, "0".to_owned()),
                (MISSILE_A, "0".to_owned()),
                (MISSILE_B, "0".to_owned()),
            ],
            "toInt32",
        ),
        put_many(
            "m_radius",
            &[
                (TARGET, POSSESSED_RADIUS.to_string()),
                (MISSILE_A, TROOPSHOT_RADIUS.to_string()),
                (MISSILE_B, TROOPSHOT_RADIUS.to_string()),
            ],
            "toInt32",
        ),
        put_many(
            "m_height",
            &[
                (TARGET, format!("p.m_height[{TARGET}]")),
                (MISSILE_A, TROOPSHOT_HEIGHT.to_string()),
                (MISSILE_B, TROOPSHOT_HEIGHT.to_string()),
            ],
            "toInt32",
        ),
        put_many(
            "m_state",
            &[
                (TARGET, format!("p.m_state[{TARGET}]")),
                (MISSILE_A, TROOPSHOT_SPAWNSTATE.to_string()),
                (MISSILE_B, TROOPSHOT_SPAWNSTATE.to_string()),
            ],
            "toInt32",
        ),
        put_many(
            "m_tics",
            &[
                (TARGET, format!("p.m_tics[{TARGET}]")),
                (MISSILE_A, TROOPSHOT_SPAWNTICS.to_string()),
                (MISSILE_B, TROOPSHOT_SPAWNTICS.to_string()),
            ],
            "toInt32",
        ),
        put_many(
            "m_target",
            &[
                (TARGET, format!("p.m_target[{TARGET}]")),
                (MISSILE_A, "0".to_owned()),
                (MISSILE_B, "0".to_owned()),
            ],
            "toUInt32",
        ),
    ];
    let mut statements: Vec<sql::Statement> = seed::row(&db, at, BEFORE, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    statements.push(sim::tick::run_statement(
        &db,
        &[Input::keys(at + 1, 0, (0, 0))],
    ));
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Hit> = fixture
        .rows(&format!(
            "SELECT tic, m_health[{TARGET}] AS target_health, unresolved, \
             m_state[{MISSILE_A}] AS missile_a_state, \
             m_state[{MISSILE_B}] AS missile_b_state, prndindex \
             FROM {db}.native_state WHERE tic IN ({at}, {}) ORDER BY tic",
            at + 1
        ))
        .await;
    fixture.finish().await;
    assert_eq!(rows.len(), 2, "the seeded row and the tic run from it");
    let before = &rows[0];
    let after = &rows[1];
    assert_eq!(before.tic, at, "the seeded row comes first");
    assert_eq!(after.tic, at + 1, "and the tic it ran comes second");

    assert_eq!(after.unresolved, 0, "both hits resolve");
    // `P_ExplodeMissile`'s own death frame (`S_TBALL2`, `states.tsv` row
    // 99): both fireballs explode, proving both connected rather than one
    // alone silently losing its own effect to the other's.
    const TBALL2: i32 = 99;
    assert_eq!(
        (after.missile_a_state, after.missile_b_state),
        (TBALL2, TBALL2),
        "both fireballs' own impacts run"
    );
    // Each fireball does one to eight times three, so two of them move the
    // target's own health by six to forty-eight. A number under that would
    // mean the second missile's own call overwrote the first's rather than
    // building on it.
    let taken = TARGET_HEALTH - after.target_health;
    assert!(
        (6..=48).contains(&taken),
        "two hits, each three to twenty-four: {taken}"
    );
    // Neither of the target's own draws or the two hits' own pain rolls
    // falls (its own z sits level with both missiles, well under
    // `FALL_HEIGHT`), so each missile draws exactly three: its own
    // damage roll, the pain roll, and `P_ExplodeMissile`'s own. Nothing
    // else on the level's own list wakes at gametic 40, so the tic's own
    // total is the two missiles' own six and nothing more.
    assert_eq!(
        after.prndindex,
        before.prndindex.wrapping_add(6),
        "each missile draws three: its own damage roll, the pain roll, and the explosion's"
    );
}
