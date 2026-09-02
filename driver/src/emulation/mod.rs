//! Emulation mode: driving the RV32IM CPU that lives in SQL.
//!
//! Loading the ROM image into `ram`, seeding the reset state, decoding the
//! text region, the resumable batch loop, and the differential run against
//! the reference emulator. Every instruction is fetched, decoded and executed
//! by SQL; these modules issue statements and read results back.

pub mod bootstrap;
pub mod decode;
pub mod diff;
pub mod fold_result;
pub mod preflight;
pub mod rom;
pub mod run;
