//! The batch fold and commit-flush SQL text the SQL CPU executes.
//!
//! Every function here builds SQL expression or statement text; none of
//! them execute anything. The generated text is byte-identical to what the
//! Python originals produced for the same inputs, which keeps the
//! compiled-expression cache key stable and is what [`crate::fold`]'s and
//! [`crate::commit`]'s golden tests verify.

pub mod commit;
pub mod config;
