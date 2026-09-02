//! Scaffolding the driver's native live tests share.
//!
//! Each test binary compiles its own copy, so an item only one of them
//! needs is dead code in the others.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use clickdoom_driver::client::{ConnArgs, Db};
use clickdoom_driver::native::plan;
use clickdoom_native::wad::Wad;

/// The map every live test loads, the demo that drives it, and the sky the
/// episode carries.
pub const MAP: &str = "E1M7";
pub const DEMO: &str = "DEMO3";
pub const SKY: &str = "SKY1";

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Where the server is. Read from `CLICKHOUSE_HOST` and
/// `CLICKHOUSE_HTTP_PORT`, defaulting to `localhost:8123`, with the
/// password taken from the environment the way the binary takes it.
pub fn conn_args(database: &str) -> ConnArgs {
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

/// The one committed probe file. `refemu/tests/probe_fixture.rs` holds the
/// directory to one file per ROM, so a glob names it without repeating the
/// ROM's hash here.
pub fn committed_fixture() -> PathBuf {
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

/// The shareware WAD the repository ships.
pub fn doom1() -> Vec<u8> {
    std::fs::read(repo_root().join("rom/wad/doom1.wad")).expect("rom/wad/doom1.wad is committed")
}

/// A private database with the level loaded into it, named for this process
/// and the case, and the connection that loaded it.
///
/// The connection stays on `default`, because the database the phases name
/// is what their first statement creates.
pub async fn loaded(case: &str) -> (String, Db) {
    let database = format!("clickdoom_driver_native_{}_{case}", std::process::id());
    let admin = conn_args("default").connect();
    drop_database(&admin, &database).await;

    let bytes = doom1();
    let wad = Wad::parse(&bytes).expect("the WAD parses");
    let phases = plan::level_phases(&database, &wad, MAP, DEMO, SKY)
        .expect("the demo has a committed melt schedule");
    plan::run(&admin, &phases)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    (database, admin)
}

pub async fn drop_database(db: &Db, database: &str) {
    db.run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}
