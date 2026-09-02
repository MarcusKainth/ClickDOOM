//! Vertical doors, from `p_doors.c`.
//!
//! A door is a sector thinker that drives the sector's ceiling with
//! `T_MovePlane` and branches on what that answers. The use key makes one
//! through `EV_VerticalDoor`, which reads the sector behind the line the
//! press reached.

use super::plane::{self, result};

/// `p_doors.c`: the `vldoor_e` values.
pub mod kind {
    pub const NORMAL: i64 = 0;
    pub const CLOSE30_THEN_OPEN: i64 = 1;
    pub const CLOSE: i64 = 2;
    pub const OPEN: i64 = 3;
    pub const RAISE_IN_5_MINS: i64 = 4;
    pub const BLAZE_RAISE: i64 = 5;
    pub const BLAZE_OPEN: i64 = 6;
    pub const BLAZE_CLOSE: i64 = 7;
}

/// `p_doors.c`
pub const VDOORSPEED: i64 = 2 << 16;
pub const VDOORWAIT: i64 = 150;
/// `i_timer.h`
const TICRATE: i64 = 35;
/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;

/// Where each part of a door's tic sits in the answer.
pub mod ran {
    /// The direction the door is left going.
    pub const DIRECTION: usize = 1;
    /// What is left of the wait at the top or the bottom.
    pub const COUNT: usize = 2;
    /// The kind the door is left as, which the five minute door changes.
    pub const KIND: usize = 3;
    /// 1 when the door comes off the thinker list this tic.
    pub const DONE: usize = 4;
    /// 1 when the tic asked for something this cannot do.
    pub const UNRESOLVED: usize = 5;
}

/// The door's own fields, as expressions over the thinker being run.
pub struct Door<'a> {
    pub kind: &'a str,
    pub direction: &'a str,
    pub count: &'a str,
    pub wait: &'a str,
}

/// `T_VerticalDoor`, given what `T_MovePlane` answered this tic.
///
/// Waiting is the whole of the tic when the direction says so, and the
/// plane only moves when it does not, so the caller asks `T_MovePlane`
/// only for a door that is going somewhere.
pub fn vertical_door(door: &Door<'_>, moved: &str) -> String {
    let kind = format!("toInt64({})", door.kind);
    let direction = format!("toInt64({})", door.direction);
    let count = format!("toInt64({}) - 1", door.count);
    let res = format!("({moved}).{}", plane::moved::RESULT);
    // Waiting at the top, and the initial wait a five minute door starts
    // on, both run the count down and act when it reaches zero.
    let fires = format!("({count} = 0)");
    let direction_now = format!(
        "toInt64(multiIf(\
         {direction} = 0 AND {fires} AND {kind} IN ({BLAZE_RAISE}, {NORMAL}), -1, \
         {direction} = 0 AND {fires} AND {kind} = {CLOSE30}, 1, \
         {direction} = 2 AND {fires} AND {kind} = {FIVE_MINS}, 1, \
         {direction} = -1 AND {res} = {PASTDEST} AND {kind} = {CLOSE30}, 0, \
         {direction} = -1 AND {res} = {CRUSHED} AND {kind} NOT IN ({BLAZE_CLOSE}, {CLOSE}), 1, \
         {direction} = 1 AND {res} = {PASTDEST} AND {kind} IN ({BLAZE_RAISE}, {NORMAL}), 0, \
         {direction}))",
        BLAZE_RAISE = kind::BLAZE_RAISE,
        NORMAL = kind::NORMAL,
        CLOSE30 = kind::CLOSE30_THEN_OPEN,
        FIVE_MINS = kind::RAISE_IN_5_MINS,
        BLAZE_CLOSE = kind::BLAZE_CLOSE,
        CLOSE = kind::CLOSE,
        PASTDEST = result::PASTDEST,
        CRUSHED = result::CRUSHED,
    );
    let count_now = format!(
        "toInt64(multiIf(\
         {direction} IN (0, 2), {count}, \
         {direction} = -1 AND {res} = {PASTDEST} AND {kind} = {CLOSE30}, {}, \
         {direction} = 1 AND {res} = {PASTDEST} AND {kind} IN ({BLAZE_RAISE}, {NORMAL}), {}, \
         toInt64({})))",
        TICRATE * 30,
        door.wait,
        door.count,
        BLAZE_RAISE = kind::BLAZE_RAISE,
        NORMAL = kind::NORMAL,
        CLOSE30 = kind::CLOSE30_THEN_OPEN,
        PASTDEST = result::PASTDEST,
    );
    // The five minute door becomes a normal one when its wait runs out.
    let kind_now = format!(
        "toInt64(if({direction} = 2 AND {fires} AND {kind} = {FIVE_MINS}, {NORMAL}, {kind}))",
        FIVE_MINS = kind::RAISE_IN_5_MINS,
        NORMAL = kind::NORMAL,
    );
    // The door leaves the list when it finishes closing, or when a door
    // that only opens reaches the top.
    let done = format!(
        "toUInt8({res} = {PASTDEST} AND (\
         ({direction} = -1 AND {kind} IN ({BLAZE_RAISE}, {BLAZE_CLOSE}, {NORMAL}, {CLOSE})) OR \
         ({direction} = 1 AND {kind} IN ({CLOSE30}, {BLAZE_OPEN}, {OPEN}))))",
        BLAZE_RAISE = kind::BLAZE_RAISE,
        BLAZE_CLOSE = kind::BLAZE_CLOSE,
        NORMAL = kind::NORMAL,
        CLOSE = kind::CLOSE,
        CLOSE30 = kind::CLOSE30_THEN_OPEN,
        BLAZE_OPEN = kind::BLAZE_OPEN,
        OPEN = kind::OPEN,
        PASTDEST = result::PASTDEST,
    );
    format!("({direction_now}, {count_now}, {kind_now}, {done}, toUInt8(0))")
}

/// `EV_VerticalDoor`: what the use key makes when it reaches `line`.
///
/// The engine only ever reads the back side's sector, because `side` is
/// nailed to 0 and it takes `sidenum[side ^ 1]`.
pub struct Opening<'a> {
    pub line: &'a str,
    pub line_special: &'a str,
    pub line_back: &'a str,
    pub sec_specialdata: &'a str,
    pub sec_ceilingheight: &'a str,
}

/// Where each part of the answer sits.
pub mod opened {
    /// The sector the door drives, or -1 when the press does nothing.
    pub const SECTOR: usize = 1;
    pub const KIND: usize = 2;
    pub const DIRECTION: usize = 3;
    pub const SPEED: usize = 4;
    pub const TOPHEIGHT: usize = 5;
    /// 1 when the line's special is spent.
    pub const CLEARS: usize = 6;
    /// The thinker the press turned around instead of making a new one.
    pub const REOPENS: usize = 7;
    pub const UNRESOLVED: usize = 8;
}

/// The manual door specials, in the order `EV_VerticalDoor` reads them.
///
/// The locked ones need a key the press does not carry here, so a press
/// that reaches one leaves the tic unresolved.
pub fn locked(special: &str) -> String {
    format!("{special} IN (26, 27, 28, 32, 33, 34)")
}

/// What the press does, given the sector behind the line.
pub fn opening(door: &Opening<'_>, lowest_ceiling: &str) -> String {
    let special = format!("toInt64({}[1 + {}])", door.line_special, door.line);
    let sector = format!("toInt32({}[1 + {}])", door.line_back, door.line);
    let held = format!("toInt64({}[1 + {sector}])", door.sec_specialdata);
    // A door already on the sector is turned around rather than remade,
    // but only for the specials the engine lists.
    let reuse = format!("{held} != 0 AND {special} IN (1, 26, 27, 28, 117)");
    let kind = format!(
        "toInt64(multiIf({special} IN (1, 26, 27, 28), {NORMAL}, \
         {special} IN (31, 32, 33, 34), {OPEN}, \
         {special} = 117, {BLAZE_RAISE}, \
         {special} = 118, {BLAZE_OPEN}, -1))",
        NORMAL = kind::NORMAL,
        OPEN = kind::OPEN,
        BLAZE_RAISE = kind::BLAZE_RAISE,
        BLAZE_OPEN = kind::BLAZE_OPEN,
    );
    let speed = format!(
        "toInt64(if({special} IN (117, 118), {}, {VDOORSPEED}))",
        VDOORSPEED * 4
    );
    format!(
        "multiIf(\
         {} OR {sector} < 0, \
         (toInt32(-1), toInt64(0), toInt64(0), toInt64(0), toInt64(0), toUInt8(0), \
         toInt64(0), toUInt8(1)), \
         {reuse}, (toInt32(-1), toInt64(0), toInt64(0), toInt64(0), toInt64(0), toUInt8(0), \
         {held}, toUInt8(0)), \
         ({sector}, {kind}, toInt64(1), {speed}, \
         toInt64({lowest_ceiling}) - {}, \
         toUInt8({special} IN (31, 32, 33, 34, 118)), toInt64(0), toUInt8(0)))",
        locked(&special),
        4 * FRACUNIT,
    )
}
