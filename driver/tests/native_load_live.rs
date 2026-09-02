//! Live proof for `clickdoom native load`, run against a real ClickHouse
//! server.
//!
//! Three things, all of which need a server to show:
//!
//!   * a level load fills the tables the renderer reads, and loading twice
//!     leaves the same row counts as loading once;
//!   * SQL turns the committed melt passes into the running total the
//!     renderer takes as `melt_step`;
//!   * a probe file loads through the staging table into `native_state`,
//!     including the -1 the probe writes into a column the schema declares
//!     unsigned, and the committed fixture is the file it is shown on.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`).

#![cfg(feature = "clickhouse-tests")]

use std::path::{Path, PathBuf};

use clickdoom_driver::client::{ConnArgs, Db};
use clickdoom_driver::native::{melt, plan, probe};
use clickdoom_native::sql::{self, Statement, probe as shape};
use clickdoom_native::{load, wad::Wad};

/// The map, demo and sky every case here loads.
const MAP: &str = "E1M7";
const DEMO: &str = "DEMO3";
const SKY: &str = "SKY1";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn conn_args(database: &str) -> ConnArgs {
    ConnArgs {
        host: std::env::var("CLICKHOUSE_HOST").unwrap_or_else(|_| "localhost".to_owned()),
        port: std::env::var("CLICKHOUSE_HTTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8123),
        user: "default".to_owned(),
        database: database.to_owned(),
        password: None,
    }
}

/// A private database per case, named for this process and the case, so
/// nothing here touches a shared one.
struct Fixture {
    database: String,
    db: Db,
}

impl Fixture {
    async fn create(case: &str) -> Fixture {
        let database = format!("clickdoom_native_load_{}_{case}", std::process::id());
        let db = conn_args("default").connect();
        db.run(&format!("DROP DATABASE IF EXISTS {database}"))
            .await
            .expect("the database is dropped");
        Fixture { database, db }
    }

    async fn finish(self) {
        self.db
            .run(&format!("DROP DATABASE IF EXISTS {}", self.database))
            .await
            .expect("the database is dropped");
    }

    async fn scalar<T>(&self, sql: &str) -> T
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        self.db
            .fetch_one::<T>(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    async fn rows<T>(&self, sql: &str) -> Vec<T>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        self.db
            .fetch_all::<T>(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    /// Every row the database holds, over every table. One number that
    /// moves when any load writes twice.
    async fn total_rows(&self) -> u64 {
        self.scalar(&format!(
            "SELECT toUInt64(ifNull(sum(total_rows), 0)) FROM system.tables \
             WHERE database = '{}'",
            self.database
        ))
        .await
    }

    async fn run_plan(&self, phases: &[plan::Phase]) {
        plan::run(&self.db, phases)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }
}

/// The phases `clickdoom native load` issues, built the same way.
fn level_phases(database: &str, wad: &Wad<'_>) -> Vec<plan::Phase> {
    vec![
        plan::Phase::new(
            "empty",
            sql::schema_tables()
                .into_iter()
                .map(|table| Statement::sql(format!("TRUNCATE TABLE IF EXISTS {database}.{table}")))
                .collect(),
        ),
        plan::Phase::new("base", load::plan(database, wad)),
        plan::Phase::new("level", sql::level_statements(database, MAP, DEMO)),
        plan::Phase::new("render", sql::render_statements(database, SKY)),
        plan::Phase::new("sim", sql::sim::load_statements(database)),
        plan::Phase::new(
            "melt",
            melt::load_statements(database, DEMO).expect("demo3 has a committed schedule"),
        ),
    ]
}

#[tokio::test]
async fn a_level_load_fills_the_tables_and_loading_twice_changes_nothing() {
    let bytes = std::fs::read(repo_root().join("rom/wad/doom1.wad")).expect("the WAD is committed");
    let wad = Wad::parse(&bytes).expect("the WAD parses");
    let fixture = Fixture::create("level").await;
    let phases = level_phases(&fixture.database, &wad);

    fixture.run_plan(&phases).await;
    let first = fixture.total_rows().await;
    assert!(first > 0, "the load wrote nothing");
    for table in [
        "wad_lumps",
        "states",
        "lv_segs",
        "tex_composite",
        "rt_yslope",
    ] {
        let count: u64 = fixture
            .scalar(&format!("SELECT count() FROM {}.{table}", fixture.database))
            .await;
        assert!(count > 0, "{table} is empty after a load");
    }

    fixture.run_plan(&phases).await;
    assert_eq!(
        fixture.total_rows().await,
        first,
        "loading twice doubled a table"
    );

    fixture.finish().await;
}

/// The renderer takes the running total, not the per-frame count. Frame 20
/// standing at 22 is what `native/tests/render_live.rs` renders frame 20
/// with, so the two agree on where the wipe has got to.
#[tokio::test]
async fn the_melt_schedule_carries_the_running_total_sql_computed() {
    let fixture = Fixture::create("melt").await;
    fixture
        .run_plan(&[
            plan::Phase::new("schema", sql::schema_statements(&fixture.database)),
            plan::Phase::new(
                "melt",
                melt::load_statements(&fixture.database, DEMO).expect("a committed schedule"),
            ),
        ])
        .await;

    let steps: Vec<(u32, u8)> = fixture
        .rows(&format!(
            "SELECT frame, melt_step FROM {}.{} ORDER BY frame",
            fixture.database,
            melt::TABLE
        ))
        .await;
    assert_eq!(steps.first(), Some(&(0, 1)));
    assert_eq!(steps.iter().find(|(f, _)| *f == 1), Some(&(1, 3)));
    assert_eq!(steps.iter().find(|(f, _)| *f == 20), Some(&(20, 22)));
    assert_eq!(steps.last(), Some(&(39, 41)), "the melt's last frame");

    fixture.finish().await;
}

/// The one committed probe file. `refemu/tests/probe_fixture.rs` holds it to
/// one per ROM, so a glob names it without pinning the hash in its name.
fn committed_fixture() -> PathBuf {
    let dir = repo_root().join("refemu/probe/fixtures");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "tsv"))
        .collect();
    found.sort();
    match found.len() {
        1 => found.remove(0),
        _ => panic!(
            "{} holds {} probe files, not one",
            dir.display(),
            found.len()
        ),
    }
}

#[tokio::test]
async fn the_committed_probe_fixture_loads_and_keys_on_the_gametic() {
    let fixture = Fixture::create("probe").await;
    fixture
        .run_plan(&[plan::Phase::new(
            "schema",
            sql::schema_statements(&fixture.database),
        )])
        .await;

    let path = committed_fixture();
    let loaded = probe::load(&fixture.db, &fixture.database, &path)
        .await
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(loaded.rows > 0, "the fixture carried no rows");
    assert!(
        loaded.tics <= loaded.rows,
        "a tic cannot cover more rows than the file holds"
    );

    let staged: u64 = fixture
        .scalar(&format!(
            "SELECT count() FROM {}.{}",
            fixture.database,
            probe::STAGING_TABLE
        ))
        .await;
    assert_eq!(staged, loaded.rows);
    let tics: u64 = fixture
        .scalar(&format!(
            "SELECT count() FROM {}.{}",
            fixture.database,
            probe::STATE_TABLE
        ))
        .await;
    assert_eq!(tics, loaded.tics, "native_state keys on the tic");

    // Every state row is the probe's own, field for field, for one field
    // that moves every tic and one the probe writes -1 into.
    let mismatched: u64 = fixture
        .scalar(&format!(
            "SELECT countIf(s.leveltime != p.leveltime OR s.m_player != CAST(p.m_player, toTypeName(s.m_player))) \
             FROM {db}.{state} AS s \
             INNER JOIN (SELECT gametic, any(leveltime) AS leveltime, any(m_player) AS m_player \
                         FROM {db}.{staging} GROUP BY gametic) AS p ON s.tic = p.gametic",
            db = fixture.database,
            state = probe::STATE_TABLE,
            staging = probe::STAGING_TABLE
        ))
        .await;
    assert_eq!(mismatched, 0, "a state row does not carry the probe's own");

    // The probe writes -1 for a mobj that is not a player, into a column
    // the schema declares unsigned. It has to survive the staging table.
    let negatives: u64 = fixture
        .scalar(&format!(
            "SELECT countIf(arrayExists(v -> v < 0, m_player)) FROM {}.{}",
            fixture.database,
            probe::STAGING_TABLE
        ))
        .await;
    assert!(negatives > 0, "the fixture has no -1 to stage");

    // Loading again replaces rather than doubles.
    let again = probe::load(&fixture.db, &fixture.database, &path)
        .await
        .expect("the second load");
    assert_eq!(again.rows, loaded.rows);
    assert_eq!(again.tics, loaded.tics);
    let tics_again: u64 = fixture
        .scalar(&format!(
            "SELECT count() FROM {}.{}",
            fixture.database,
            probe::STATE_TABLE
        ))
        .await;
    assert_eq!(tics_again, loaded.tics);

    fixture.finish().await;
}

/// A file whose columns do not name what the contract does is refused
/// before anything is written, because the rows are positional.
///
/// Which shapes are refused is `native::sql::probe`'s own contract and its
/// own unit tests. What this covers is the driver's half: the refusal
/// reaches the caller naming the file, and nothing is left in the database.
#[tokio::test]
async fn a_probe_file_that_does_not_match_the_contract_is_refused() {
    let fixture = Fixture::create("probe_shape").await;
    fixture
        .run_plan(&[plan::Phase::new(
            "schema",
            sql::schema_statements(&fixture.database),
        )])
        .await;

    let dir = std::env::temp_dir().join(format!("clickdoom-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");

    let cases = [
        ("no-header.tsv", "0\t2\tfe5d\n".to_owned()),
        (
            "unknown-column.tsv",
            "# refemu-probe 1\n# columns\tframe_index\tgametic\tfb_hash\tnot_a_column\n\
             0\t2\tfe5d\t1\n"
                .to_owned(),
        ),
        (
            "swapped.tsv",
            format!(
                "# refemu-probe 1\n# columns\t{}\n0\t2\tfe5d\n",
                swapped_columns().join("\t")
            ),
        ),
    ];
    for (name, text) in cases {
        let path = dir.join(name);
        std::fs::write(&path, &text).expect("a file");
        let error = probe::load(&fixture.db, &fixture.database, &path)
            .await
            .err()
            .unwrap_or_else(|| panic!("{name} was accepted"));
        assert!(
            matches!(error, probe::Error::Shape { .. }),
            "{name}: {error}"
        );
        assert!(
            error.to_string().contains(name),
            "{name} is not named in: {error}"
        );
    }

    let staged: u64 = fixture
        .scalar(&format!(
            "SELECT count() FROM system.tables WHERE database = '{}' AND name = '{}'",
            fixture.database,
            probe::STAGING_TABLE
        ))
        .await;
    assert_eq!(staged, 0, "a refused file left a staging table behind");

    std::fs::remove_dir_all(&dir).ok();
    fixture.finish().await;
}

/// The contract's own column list with two of its fields swapped, which is
/// the shape a positional read would get wrong without noticing.
fn swapped_columns() -> Vec<&'static str> {
    let mut names = shape::names();
    names.swap(3, 4);
    names
}
