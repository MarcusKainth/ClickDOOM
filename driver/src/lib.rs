//! The ClickDOOM driver: a persistent ClickHouse client and the subcommands
//! built on it.

pub mod bootstrap;
pub mod checkpoint;
pub mod cli;
pub mod client;
pub mod decode;
pub mod render;
pub mod rom;
pub mod sql;
