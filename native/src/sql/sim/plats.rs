//! Moving platforms, from `p_plats.c`.
//!
//! A plat drives its sector's floor between a low and a high, waiting at
//! the end of each run. Which of those it is doing is its status, and
//! `T_MovePlane`'s answer is what moves it on.

use super::plane::result;

/// `p_spec.h`: the `plat_e` values, in the order they are declared.
pub mod status {
    pub const UP: i64 = 0;
    pub const DOWN: i64 = 1;
    pub const WAITING: i64 = 2;
    pub const IN_STASIS: i64 = 3;
}

/// `p_spec.h`: the `plattype_e` values, in the order they are declared.
pub mod kind {
    pub const PERPETUAL_RAISE: i64 = 0;
    pub const DOWN_WAIT_UP_STAY: i64 = 1;
    pub const RAISE_AND_CHANGE: i64 = 2;
    pub const RAISE_TO_NEAREST_AND_CHANGE: i64 = 3;
    pub const BLAZE_DWUS: i64 = 4;
}

/// `p_spec.h`
pub const PLATWAIT: i64 = 3;
pub const PLATSPEED: i64 = 1 << 16;
/// `i_timer.h`
pub const TICRATE: i64 = 35;

/// Where each part of a plat's tic sits in the answer.
pub mod ran {
    /// The status the plat is left in.
    pub const STATUS: usize = 1;
    /// What is left of the wait.
    pub const COUNT: usize = 2;
    /// 1 when the plat comes off the thinker list this tic.
    pub const DONE: usize = 3;
}

/// The plat's own fields, as expressions over the thinker being run.
pub struct Plat<'a> {
    pub kind: &'a str,
    pub status: &'a str,
    pub count: &'a str,
    pub wait: &'a str,
    pub crush: &'a str,
    /// The floor it runs down to, and the one it runs up to.
    pub low: &'a str,
    /// Where the sector's floor stands after the move.
    pub floorheight: &'a str,
}

/// `T_PlatRaise`, given what `T_MovePlane` answered this tic.
///
/// Only `up` and `down` move a plane, so a caller asks `T_MovePlane` for
/// those and hands the answer in. `waiting` runs the count down and turns
/// the plat around when it reaches zero.
pub fn plat_raise(plat: &Plat<'_>, moved: &str) -> String {
    let kind = format!("toInt64({})", plat.kind);
    let status = format!("toInt64({})", plat.status);
    let count = format!("toInt64({}) - 1", plat.count);
    let res = format!("({moved}).{}", result_of());
    let crushed_now = format!("{res} = {} AND {} = 0", result::CRUSHED, plat.crush);
    let arrived = format!("{res} = {}", result::PASTDEST);
    // A plat that has run out of wait goes back the way it came, and a
    // plat already at the bottom goes up.
    let turns = format!(
        "({status} = {WAITING} AND {count} = 0)",
        WAITING = status::WAITING
    );
    let status_now = format!(
        "toInt64(multiIf(\
         {status} = {UP} AND {crushed_now}, {DOWN}, \
         {status} = {UP} AND {arrived}, {WAITING}, \
         {status} = {DOWN} AND {arrived}, {WAITING}, \
         {turns} AND toInt64({}) = toInt64({}), {UP}, \
         {turns}, {DOWN}, \
         {status}))",
        plat.floorheight,
        plat.low,
        UP = status::UP,
        DOWN = status::DOWN,
        WAITING = status::WAITING,
    );
    let count_now = format!(
        "toInt64(multiIf(\
         {status} = {UP} AND ({crushed_now} OR {arrived}), toInt64({}), \
         {status} = {DOWN} AND {arrived}, toInt64({}), \
         {status} = {WAITING}, {count}, \
         toInt64({})))",
        plat.wait,
        plat.wait,
        plat.count,
        UP = status::UP,
        DOWN = status::DOWN,
        WAITING = status::WAITING,
    );
    // A plat that only runs once leaves the list when it arrives at the
    // top. The perpetual one stays and keeps going.
    let done = format!(
        "toUInt8({status} = {UP} AND {arrived} AND {kind} != {PERPETUAL})",
        UP = status::UP,
        PERPETUAL = kind::PERPETUAL_RAISE,
    );
    format!("({status_now}, {count_now}, {done})")
}

/// `T_MovePlane`'s answer sits first in what it returns.
fn result_of() -> usize {
    super::plane::moved::RESULT
}
