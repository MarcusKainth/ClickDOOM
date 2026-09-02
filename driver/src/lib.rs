//! The ClickDOOM driver: a persistent ClickHouse client and the subcommands
//! built on it.
//!
//! [`emulation`] holds everything specific to running the RV32IM CPU in SQL,
//! [`native`] the transport DOOM's own simulation and renderer run on. The
//! modules beside them are shared: the client, statement splitting, the
//! checkpoint and frame-readout SQL builders, frame files, the progress line and
//! the benchmark harness.

pub mod bench;
pub mod checkpoint;
pub mod cli;
pub mod client;
pub mod emulation;
pub mod frames;
pub mod native;
pub mod render;
pub mod sql;
pub mod stats;
