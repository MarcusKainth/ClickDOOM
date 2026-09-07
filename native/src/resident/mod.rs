//! The transport native mode's own resident statements run on.
//!
//! `NATIVE.md` states the contract: each component of native mode is one
//! `INSERT ... SELECT ... FROM input(...)` kept open for a whole session,
//! with one row streamed into it per tic. [`stream`] holds that statement
//! open, [`rowbinary`] encodes the rows it takes, [`settings`] names the
//! settings it runs under and [`url`] builds the request target.
//!
//! Driving more than one of these together, and reading back what they
//! wrote, is a caller's own concern: the driver's `native::session` module
//! is the one this project has.

pub mod rowbinary;
pub mod settings;
pub mod stream;
pub mod url;

pub use settings::resident_settings;
pub use stream::{CLOSE_TIMEOUT, Endpoint, FORMAT_CLAUSE, Resident, ResidentError};
