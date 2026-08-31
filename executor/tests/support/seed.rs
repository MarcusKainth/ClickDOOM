//! Write-log seed shapes for `fold::select_only`'s `wl0` argument.
//!
//! `wl0` seeds RAM's write-log (`acc.3`) so its length can vary
//! independently of K. Nothing in normal operation can do that: the log
//! only grows by retiring stores.
//!
//! All three lanes are always seeded to the same length. They are parallel
//! arrays, and load forwarding finds an index in the address lane and
//! subscripts the value lane with it, so unequal lengths desynchronise them
//! and a forwarding load reads the wrong slot. That is a semantic change,
//! not a cost-only one.
//!
//! The generated SQL is constant in size: `arrayResize` over an empty array
//! takes the length as a number, so only its decimal digits change. A
//! literal array of `l0` elements would make parse cost a function of `l0`.

/// The address every seeded write-log entry carries. The fold clamps a real
/// load's word index to at most `ram_words - 1`, so any value at or above
/// `ram_words` can never match one, and `u32::MAX` is unmistakable in a
/// dump: a row at that word address is a seed leak, never a real store.
pub const SENTINEL_ADDR: u32 = u32::MAX;

/// The two semantically valid seed shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// An empty write-log, byte-identical to the production seed.
    Empty,
    /// All three lanes seeded to `l0`, index-aligned.
    AllLanes,
}

/// The `acc.3` initial value for `shape` at seed length `l0`.
///
/// `Shape::Empty` ignores `l0`: it is the production seed by definition,
/// and honouring `l0` there would make "empty at l0 = 80,000" mean
/// something.
pub fn seed_sql(shape: Shape, l0: u32) -> String {
    let n = match shape {
        Shape::Empty => 0,
        Shape::AllLanes => l0,
    };
    if n == 0 {
        return "tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64())".to_owned();
    }
    format!(
        "tuple(\
         arrayResize(emptyArrayUInt32(), {n}, toUInt32({SENTINEL_ADDR})), \
         arrayResize(emptyArrayUInt32(), {n}, toUInt32(0)), \
         arrayResize(emptyArrayUInt64(), {n}, toUInt64(0)))"
    )
}

/// How many entries the write-log starts with, which is what the
/// high-water-mark check sees. A seeded run consumes that headroom, so a
/// caller has to raise `hwm` to match.
pub fn seeded_len(shape: Shape, l0: u32) -> u32 {
    match shape {
        Shape::Empty => 0,
        Shape::AllLanes => l0,
    }
}
