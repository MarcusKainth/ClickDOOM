//! The load plan against a real ClickHouse server.
//!
//! `plan_golden.rs` proves the statement list is what it was; it never
//! issues one. This issues every one of them and checks what reaches the
//! server: the schema the DDL declares, the WAD's lumps byte for byte, and
//! the engine's constant tables row for row.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::{load, sql::Statement, tables, wad::Wad};
use clickdoom_spec::sha256_hex;
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;

/// Loads a fresh database and hands it to `check`, then drops it whether
/// the check passed or not.
async fn loaded(case: &str, check: impl AsyncFnOnce(&Fixture, &Wad<'_>)) {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create(case).await;
    let plan = load::plan(&fixture.database, &wad);
    let result = fixture.execute(&plan).await;
    if let Err(error) = result {
        fixture.finish().await;
        panic!("{error}");
    }
    check(&fixture, &wad).await;
    fixture.finish().await;
}

#[tokio::test]
async fn the_plan_creates_every_table_the_schema_declares() {
    loaded("schema", async |fixture, _| {
        let declared: Vec<String> = load::plan(&fixture.database, &Wad::parse(HEADER).unwrap())
            .iter()
            .filter_map(|s| table_name(s, &fixture.database))
            .collect();
        let created: Vec<String> = fixture
            .rows(&format!(
                "SELECT name FROM system.tables WHERE database = '{}' ORDER BY name",
                fixture.database
            ))
            .await;
        let mut expected = declared.clone();
        expected.sort();
        assert_eq!(created, expected);
    })
    .await;
}

/// A WAD with a header and no lumps, for the statement list alone.
const HEADER: &[u8] = b"IWAD\x00\x00\x00\x00\x0c\x00\x00\x00";

fn table_name(statement: &Statement, database: &str) -> Option<String> {
    let prefix = format!("CREATE TABLE IF NOT EXISTS {database}.");
    let rest = statement.sql.strip_prefix(&prefix)?;
    Some(rest.split(['\n', ' ', '(']).next()?.to_owned())
}

/// One lump as the server holds it. The bytes come back as a digest
/// rather than as themselves: the WAD is 4 MB, and a digest that matches
/// the one taken over the file says the same thing.
#[derive(Row, Deserialize)]
struct LumpRow {
    id: u32,
    name: String,
    map_marker: String,
    len: u64,
    digest: String,
}

#[tokio::test]
async fn every_lump_arrives_with_its_bytes_unchanged() {
    loaded("lumps", async |fixture, wad| {
        assert_eq!(
            fixture.count("wad_lumps").await,
            wad.lumps().len() as u64,
            "lump count"
        );
        let rows: Vec<LumpRow> = fixture
            .rows(&format!(
                "SELECT id, name, map_marker, length(bytes) AS len, \
                 lower(hex(SHA256(bytes))) AS digest \
                 FROM {}.wad_lumps ORDER BY id",
                fixture.database
            ))
            .await;
        assert_eq!(rows.len(), wad.lumps().len());
        for (row, lump) in rows.iter().zip(wad.lumps()) {
            let at = format!("lump {} ({})", lump.index, lump.name);
            assert_eq!(row.id, lump.index, "{at} id");
            assert_eq!(row.name, lump.name, "{at} name");
            assert_eq!(row.map_marker, lump.map_marker, "{at} marker");
            assert_eq!(row.len as usize, lump.bytes.len(), "{at} length");
            assert_eq!(row.digest, sha256_hex(lump.bytes), "{at} bytes");
        }
    })
    .await;
}

#[tokio::test]
async fn every_constant_table_arrives_with_its_rows() {
    loaded("tables", async |fixture, _| {
        for embedded in &tables::TABLES {
            let expected = tables::table(embedded.name).unwrap().rows.len() as u64;
            assert_eq!(
                fixture.count(embedded.name).await,
                expected,
                "{} row count",
                embedded.name
            );
        }
    })
    .await;
}

/// The types the schema picks have to hold the values the tables carry.
/// A column too narrow would have been clamped or refused on insert, so
/// reading the extremes back is what says the choice was right.
#[tokio::test]
async fn the_column_types_hold_the_values_the_tables_carry() {
    loaded("types", async |fixture, _| {
        let db = &fixture.database;
        let (min, max): (i32, i32) = fixture
            .scalar(&format!(
                "SELECT min(value), max(value) FROM {db}.finetangent"
            ))
            .await;
        assert_eq!((min, max), (-170_910_304, 170_910_304));
        let peak: u32 = fixture
            .scalar(&format!("SELECT max(value) FROM {db}.tantoangle"))
            .await;
        assert_eq!(peak, 0x2000_0000);
        let flags: i32 = fixture
            .scalar(&format!("SELECT flags FROM {db}.mobjinfo WHERE id = 0"))
            .await;
        assert_eq!(flags, 2 | 4 | 0x400 | 0x800 | 0x200_0000);
        let terminator: i32 = fixture
            .scalar(&format!(
                "SELECT istexture FROM {db}.animdefs ORDER BY id DESC LIMIT 1"
            ))
            .await;
        assert_eq!(terminator, -1);
    })
    .await;
}

/// The two output tables are `Join` engines keyed on the tic and the
/// frame. A second write for one key has to replace the first, because a
/// resumed session re-runs the tic it stopped on.
#[tokio::test]
async fn a_rerun_tic_replaces_its_row() {
    loaded("join", async |fixture, _| {
        let db = &fixture.database;
        for leveltime in [11, 22] {
            fixture
                .execute(&[Statement::sql(format!(
                    "INSERT INTO {db}.native_state (tic, leveltime) VALUES (7, {leveltime})"
                ))])
                .await
                .unwrap();
        }
        let value: i32 = fixture
            .scalar(&format!(
                "SELECT joinGet('{db}.native_state', 'leveltime', toUInt32(7))"
            ))
            .await;
        assert_eq!(value, 22, "the second write did not replace the first");
        let missing: i32 = fixture
            .scalar(&format!(
                "SELECT joinGet('{db}.native_state', 'leveltime', toUInt32(8))"
            ))
            .await;
        assert_eq!(missing, 0, "an absent tic reads as the column default");
    })
    .await;
}

/// Re-running the schema against a loaded database changes nothing. The
/// driver issues it on every start.
#[tokio::test]
async fn the_schema_is_idempotent() {
    loaded("idempotent", async |fixture, _| {
        let before = fixture.count("wad_lumps").await;
        fixture
            .execute(&clickdoom_native::sql::schema_statements(&fixture.database))
            .await
            .unwrap();
        assert_eq!(fixture.count("wad_lumps").await, before);
    })
    .await;
}
