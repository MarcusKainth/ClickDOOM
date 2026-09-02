//! Scaffolding the native crate's test binaries share.
//!
//! Each test binary compiles its own copy, so an item only one of them
//! needs is dead code in the others.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

#[cfg(feature = "clickhouse-tests")]
pub mod db;

/// The shareware WAD the repository ships.
pub fn doom1_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rom/wad/doom1.wad")
}

pub fn doom1() -> Vec<u8> {
    std::fs::read(doom1_path()).expect("rom/wad/doom1.wad is committed")
}

#[cfg(feature = "clickhouse-tests")]
pub mod patch;

#[cfg(feature = "clickhouse-tests")]
pub mod probe;
pub mod spawn;
pub mod ticker;

/// The map every live test loads, the demo that drives it, and the sky the
/// episode carries.
pub const MAP: &str = "E1M7";
pub const DEMO: &str = "DEMO3";
pub const SKY: &str = "SKY1";
