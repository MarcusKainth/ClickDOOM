//! The ClickDOOM driver: a persistent ClickHouse client and the subcommands
//! built on it.

pub mod bench;
pub mod bootstrap;
pub mod checkpoint;
pub mod cli;
pub mod client;
pub mod decode;
pub mod diff;
pub mod fold_result;
pub mod frames;
pub mod preflight;
pub mod render;
pub mod rom;
pub mod run;
pub mod sql;
