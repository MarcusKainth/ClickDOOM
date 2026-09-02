//! The transport native mode runs on.
//!
//! `NATIVE.md` states the contract: each component of native mode is one
//! `INSERT ... SELECT ... FROM input(...)` kept open for a whole session,
//! with one row streamed into it per tic. [`rowbinary`] encodes the rows
//! that statement takes, [`settings`] names the settings it runs under and
//! [`url`] builds the request target.

pub mod rowbinary;
pub mod settings;
pub mod url;
