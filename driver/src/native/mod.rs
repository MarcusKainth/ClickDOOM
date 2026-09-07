//! Driving native mode's own resident statements.
//!
//! `NATIVE.md` states the contract, and `clickdoom_native::resident` carries
//! the one implementation of the wire protocol it describes: opening a
//! statement, streaming one row per tic, reading the `Join` table it
//! writes back. [`session`] drives the three of one session together: the
//! simulation's own two, chained through `native_stage`, and the renderer.
//!
//! The one-off side is beside it: [`plan`] issues the statements that load a
//! database, [`probe`] loads the reference emulator's state rows, [`melt`]
//! the screen wipe's schedule, and [`schedule`] reads back which frames a
//! run renders and what each one draws from. [`refusal`] reads back the
//! tic a run stopped at, where `unresolved` or `unimplemented` said one
//! could not be produced exactly. [`schema`] stands between a database an
//! older binary loaded and one this binary's own statements can read: a
//! load without `--fresh` refuses a column that moved, and
//! [`Session::open`] refuses a database whose schema hash is not this
//! binary's own.
//!
//! A paced run adds [`pace`], the 35 Hz tic clock, and [`window`], which
//! puts the frame SQL produced on the screen.

pub mod melt;
pub mod pace;
pub mod plan;
pub mod probe;
pub mod refusal;
pub mod schedule;
pub mod schema;
pub mod session;
pub mod window;

pub use clickdoom_native::resident::{Resident, ResidentError};
pub use refusal::Refusal;
pub use session::{Frame, Recovery, Session, SessionError, Waited};
