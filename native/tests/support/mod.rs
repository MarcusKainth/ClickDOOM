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

/// The map every live test loads.
pub const MAP: &str = "E1M7";
