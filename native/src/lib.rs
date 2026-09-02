//! Native mode: the DOOM engine's own architecture, as ClickHouse SQL.
//!
//! Every function here builds SQL text or decodes the WAD directory into rows;
//! none of them execute anything. The driver executes, and `NATIVE.md` states
//! what the text has to do.

pub mod wad;
