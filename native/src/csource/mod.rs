//! Reading constant data out of the vendored engine's C source.
//!
//! The engine's tables are program data: `states`, `mobjinfo`, the
//! trigonometry lookups, the random table. They belong in ClickHouse as
//! rows, and the only defensible way to get them there is to read them
//! from the source the ROM is built from.
//!
//! This is not a C compiler. It lexes, evaluates integer constant
//! expressions against the enumerators and `#define`s it finds, and reads
//! braced initializers. Anything it cannot evaluate is an error rather
//! than a guess.

pub mod error;
pub mod expr;
pub mod init;
pub mod lex;
pub mod symbols;

pub use error::CError;
pub use init::{Array, Ctx, Node};
pub use lex::{Tok, Token, lex};
pub use symbols::Symbols;
