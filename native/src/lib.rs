//! Native mode: the DOOM engine's own architecture, as ClickHouse SQL.
//!
//! Everything under [`sql`], [`load`], [`tables`] and [`wad`] builds SQL
//! text or decodes the WAD directory into rows; none of it executes
//! anything. [`resident`] is the exception: the wire protocol a resident
//! statement runs on is itself part of native mode's own contract, so this
//! crate carries the one implementation of it rather than each caller's
//! own copy. `NATIVE.md` states what both carry.

pub mod csource;
pub mod load;
pub mod resident;
pub mod sql;
pub mod tables;
pub mod wad;
