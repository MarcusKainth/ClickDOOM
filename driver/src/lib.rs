//! The ClickDOOM driver: a persistent ClickHouse client and the subcommands
//! built on it.
//!
//! [`emulation`] holds everything specific to running the RV32IM CPU in SQL.
//! The modules beside it are shared: the client, statement splitting, the
//! checkpoint and frame-readout SQL builders, frame files, and the benchmark
//! harness.

pub mod bench;
pub mod checkpoint;
pub mod cli;
pub mod client;
pub mod emulation;
pub mod frames;
pub mod render;
pub mod sql;
