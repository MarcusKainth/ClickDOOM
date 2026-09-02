//! The transport native mode runs on.
//!
//! `NATIVE.md` states the contract: each component of native mode is one
//! `INSERT ... SELECT ... FROM input(...)` kept open for a whole session,
//! with one row streamed into it per tic. [`stream`] holds that statement
//! open, [`rowbinary`] encodes the rows it takes, [`settings`] names the
//! settings it runs under and [`url`] builds the request target.
//! [`session`] drives the two statements of one session together.
//!
//! The one-off side is beside it: [`plan`] issues the statements that load a
//! database, [`probe`] loads the reference emulator's state rows, [`melt`]
//! the screen wipe's schedule, and [`schedule`] reads back which frames a
//! run renders and what each one draws from.
//!
//! A paced run adds [`pace`], the 35 Hz tic clock.

pub mod melt;
pub mod pace;
pub mod plan;
pub mod probe;
pub mod rowbinary;
pub mod schedule;
pub mod session;
pub mod settings;
pub mod stream;
pub mod url;

pub use session::{Frame, Recovery, Session, SessionError};
pub use stream::{Resident, ResidentError};
