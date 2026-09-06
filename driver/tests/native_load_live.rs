//! Live proof for `clickdoom native load`, run against a real ClickHouse
//! server.
//!
//! Five things, all of which need a server to show:
//!
//!   * a level load fills the tables the renderer reads, and loading twice
//!     leaves the same row counts as loading once;
//!   * SQL turns the committed melt passes into the running total the
//!     renderer takes as `melt_step`;
//!   * a probe file loads through the staging table into `native_state`,
//!     including the -1 the probe writes into a column the schema declares
//!     unsigned, and the committed fixture is the file it is shown on;
//!   * a database a narrower schema loaded is refused, both by a load
//!     without `--fresh` and by the hash check a session opens with;
//!   * `schema_columns` names the same columns, with the same types, that
//!     a fresh load's own `system.columns` does, in both directions.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`).

#![cfg(feature = "clickhouse-tests")]

use clickdoom_driver::client::Db;
use clickdoom_driver::native::{melt, plan, probe, schema};
use clickdoom_native::sql::{self, Statement, probe as shape};
use clickdoom_native::wad::Wad;

mod support;

use support::{DEMO, MAP, SKY, committed_fixture, conn_args, doom1};

/// The map, demo and sky every case here loads.
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
    let mut phases = vec![plan::Phase::new(
        "empty",
        sql::schema_tables()
            .into_iter()
            .map(|table| Statement::sql(format!("TRUNCATE TABLE IF EXISTS {database}.{table}")))
            .collect(),
    )];
    phases.extend(
        plan::level_phases(database, wad, MAP, DEMO, SKY)
            .expect("demo3 has a committed melt schedule"),
    );
    phases
}

#[tokio::test]
async fn a_level_load_fills_the_tables_and_loading_twice_changes_nothing() {
    let bytes = doom1();
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

/// `native_state`'s own columns, `unresolved` narrowed to `UInt8`: the
/// shape a database an older schema loaded would have left.
///
/// `Join` does not support `ALTER ... MODIFY COLUMN`, so this rebuilds the
/// table from `schema_columns` rather than narrowing one in place.
fn native_state_ddl_with_narrow_unresolved(database: &str) -> String {
    let columns: Vec<String> = clickdoom_native::sql::schema_columns()
        .into_iter()
        .filter(|(table, _, _)| *table == "native_state")
        .map(|(_, name, kind)| {
            let kind = if name == "unresolved" {
                "UInt8".to_owned()
            } else {
                kind
            };
            format!("{name} {kind}")
        })
        .collect();
    format!(
        "CREATE TABLE {database}.native_state ({}) ENGINE = Join(ANY, LEFT, tic)",
        columns.join(", ")
    )
}

/// A database this binary's own schema loaded, but with `unresolved` left
/// at the width an older schema would have declared it, and never given
/// this binary's own schema hash: the shape both `native load` (without
/// `--fresh`) and a session opened later have to refuse rather than read.
///
/// `check_columns` and `check_hash` are the driver's own functions, called
/// directly here rather than through the `clickdoom` binary: what this
/// covers is that each one actually reaches this database and reports the
/// mismatch, not the CLI plumbing around them.
#[tokio::test]
async fn a_database_a_narrower_schema_loaded_is_refused_by_load_and_by_open() {
    let fixture = Fixture::create("stale_schema").await;
    fixture
        .run_plan(&[plan::Phase::new(
            "schema",
            sql::schema_statements(&fixture.database),
        )])
        .await;
    fixture
        .db
        .run(&format!("DROP TABLE {}.native_state", fixture.database))
        .await
        .expect("dropping the table to rebuild it narrower");
    fixture
        .db
        .run(&native_state_ddl_with_narrow_unresolved(&fixture.database))
        .await
        .expect("recreating native_state with a narrow unresolved");

    let mismatch = schema::check_columns(&fixture.db, &fixture.database)
        .await
        .expect("the read succeeds")
        .unwrap_or_else(|| panic!("a narrowed unresolved was not caught"));
    assert!(
        matches!(
            &mismatch,
            schema::Mismatch::Changed { table, column, want, got, .. }
                if table == "native_state" && column == "unresolved" && want == "UInt64" && got == "UInt8"
        ),
        "{mismatch}"
    );

    // The schema was never loaded through `clickdoom native load`, so
    // `schema_hash` carries no row for a session to agree with.
    let stale = schema::check_hash(&fixture.db, &fixture.database)
        .await
        .unwrap_or_else(|| panic!("a database with no schema hash was not caught"));
    assert!(
        matches!(&stale, schema::Mismatch::Stale { database, .. } if database == &fixture.database),
        "{stale}"
    );

    fixture.finish().await;
}

/// `schema_columns`, checked column for column against a fresh load's own
/// `system.columns`, in both directions: every column `schema_columns`
/// names is one `system.columns` also names with the same type, and every
/// column `system.columns` names is one `schema_columns` also names.
///
/// This is the same comparison `check_columns` makes, run once here over
/// the whole schema rather than stopping at the first table `check_columns`
/// itself reaches, so a table nothing else in this file loads (`native_frames`,
/// whose `st_cache` is the `Tuple` that first exposed the parser's own
/// line-splitting bug) is covered too. `system.columns`' type text is
/// ClickHouse's own rendering, collapsed the same way `schema_columns`
/// already collapses a type that spans several lines, so a real
/// disagreement is not lost in one side's whitespace.
#[tokio::test]
async fn schema_columns_matches_system_columns_exactly_both_ways() {
    let bytes = doom1();
    let wad = Wad::parse(&bytes).expect("the WAD parses");
    let fixture = Fixture::create("schema_columns").await;
    fixture
        .run_plan(&level_phases(&fixture.database, &wad))
        .await;

    let rows: Vec<(String, String, String)> = fixture
        .rows(&format!(
            "SELECT table, name, type FROM system.columns WHERE database = '{}'",
            fixture.database
        ))
        .await;
    let actual: Vec<(String, String, String)> = rows
        .into_iter()
        .map(|(table, name, kind)| (table, name, sql::collapse_type(&kind)))
        .collect();
    let parsed: Vec<(String, String, String)> = sql::schema_columns()
        .into_iter()
        .map(|(table, column, kind)| (table.to_owned(), column, kind))
        .collect();

    for entry in &parsed {
        assert!(
            actual.contains(entry),
            "schema_columns names {entry:?}, a fresh load's system.columns does not"
        );
    }
    for entry in &actual {
        assert!(
            parsed.contains(entry),
            "a fresh load's system.columns names {entry:?}, schema_columns does not"
        );
    }

    fixture.finish().await;
}
