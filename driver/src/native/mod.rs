//! The transport native mode runs on.
//!
//! `NATIVE.md` states the contract: each component of native mode is one
//! `INSERT ... SELECT ... FROM input(...)` kept open for a whole session,
//! with one row streamed into it per tic. [`stream`] holds that statement
//! open, [`rowbinary`] encodes the rows it takes, [`settings`] names the
//! settings it runs under and [`url`] builds the request target.

pub mod rowbinary;
pub mod settings;
pub mod stream;
pub mod url;

pub use stream::{Resident, ResidentError};
