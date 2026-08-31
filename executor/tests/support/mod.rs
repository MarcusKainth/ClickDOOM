//! Scaffolding the executor's test binaries share.
//!
//! Each test binary compiles its own copy, so an item only one of them
//! needs is dead code in the others.
#![allow(dead_code)]

pub mod insn;
pub mod reference;
pub mod seed;

#[cfg(feature = "clickhouse-tests")]
pub mod db;
#[cfg(feature = "clickhouse-tests")]
pub mod fixture;
#[cfg(feature = "clickhouse-tests")]
pub mod fold_case;

/// Where RAM starts, as a byte address and as a `ram`/`decoded` word
/// address. Both tables key on absolute word addresses.
pub const RAM_BASE: u32 = clickdoom_spec::RAM_BASE;
pub const RAM_BASE_WORD: u32 = RAM_BASE >> 2;
