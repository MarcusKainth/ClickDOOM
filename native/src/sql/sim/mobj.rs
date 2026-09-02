//! What a thing does with its momentum, from `p_mobj.c`.

use super::inter;
use super::map::{self, World, answer};
use crate::sql::bind;

/// `p_mobj.c`: the fastest one step carries, when the move is split.
const MAXMOVE: i64 = 30 << 16;
/// `p_mobj.c`: below this a thing that is not being pushed stops dead.
const STOPSPEED: i64 = 0x1000;
/// `p_mobj.c`: what is left of the momentum after a tic on the ground.
const FRICTION: i64 = 0xe800;
/// `p_local.h`
const GRAVITY: i64 = 1 << 16;
const VIEWHEIGHT: i64 = 41 << 16;

/// `p_mobj.h`
const MF_NOGRAVITY: i64 = 512;

/// Where each field of the move loop's state sits in its tuple.
pub mod moving {
    pub const X: usize = 1;
    pub const Y: usize = 2;
    pub const XMOVE: usize = 3;
    pub const YMOVE: usize = 4;
    pub const GOING: usize = 5;
    pub const FLOORZ: usize = 6;
    pub const CEILINGZ: usize = 7;
    pub const SUBSECTOR: usize = 8;
    pub const BLOCKED: usize = 9;
    pub const PICKED_UP: usize = 10;
    pub const ALIVE: usize = 11;
}

/// The thing whose momentum is being spent, as expressions.
pub struct Mover<'a> {
    pub slot: &'a str,
    pub radius: &'a str,
    pub height: &'a str,
    pub z: &'a str,
    pub flags: &'a str,
    pub is_player: &'a str,
    /// The momentum the push left, before the loop clamps it.
    pub momx: &'a str,
    pub momy: &'a str,
    pub x: &'a str,
    pub y: &'a str,
    pub floorz: &'a str,
    pub ceilingz: &'a str,
    pub subsector: &'a str,
}

/// What a pickup needs to read and to leave behind.
pub struct Pickups<'a> {
    pub m_sprite: &'a str,
    pub m_flags: &'a str,
    pub m_z: &'a str,
    pub skill: &'a str,
    /// The accumulator the first pickup starts from.
    pub start: &'a str,
    /// Every mobj slot, all alive, as the loop starts.
    pub alive: &'a str,
}

/// How many steps `P_XYMovement`'s loop takes.
///
/// It halves the move while either axis is over half of `MAXMOVE`, and one
/// halving puts both under it, so a thing that is moving takes one step or
/// two and a thing that is not takes none.
pub fn steps(momx: &str, momy: &str) -> String {
    format!(
        "toUInt32(multiIf({momx} = 0 AND {momy} = 0, 0, \
         {clamped_x} > {half} OR {clamped_y} > {half}, 2, 1))",
        half = MAXMOVE / 2,
        clamped_x = clamp(momx),
        clamped_y = clamp(momy),
    )
}

/// `P_XYMovement`'s move loop, as a fold whose accumulator carries where
/// the thing has got to and what it has picked up on the way.
///
/// The loop's steps depend on each other: the second reads the position
/// the first reached and the world it left, because
/// `P_TouchSpecialThing` takes a thing off the blockmap as it picks it up.
/// `P_TryMove` appears once inside the step.
pub fn xy_movement(mover: &Mover<'_>, world: &World<'_>, pickups: &Pickups<'_>) -> String {
    let held = |field: usize| format!("move_at.{field}");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value(
        "st_split",
        format!(
            "toUInt8({} = 1 AND ({} > {half} OR {} > {half}))",
            held(moving::GOING),
            held(moving::XMOVE),
            held(moving::YMOVE),
            half = MAXMOVE / 2
        ),
    );
    value(
        "st_tryx",
        format!(
            "toInt32(toInt64({}) + if(st_split = 1, intDiv(toInt64({}), 2), toInt64({})))",
            held(moving::X),
            held(moving::XMOVE),
            held(moving::XMOVE)
        ),
    );
    value(
        "st_tryy",
        format!(
            "toInt32(toInt64({}) + if(st_split = 1, intDiv(toInt64({}), 2), toInt64({})))",
            held(moving::Y),
            held(moving::YMOVE),
            held(moving::YMOVE)
        ),
    );
    let asking = map::asking(
        mover.slot,
        "st_tryx",
        "st_tryy",
        mover.radius,
        mover.height,
        mover.z,
        mover.flags,
        mover.is_player,
    );
    value(
        "st_answer",
        format!("{}[1]", map::try_moves(&format!("[{asking}]"), world)),
    );
    value(
        "st_moved",
        format!(
            "toUInt8({} = 1 AND st_answer.{} = 1)",
            held(moving::GOING),
            answer::OK
        ),
    );
    // `PIT_CheckThing` picks a thing up as it walks past it, whether or
    // not the move it was testing is then allowed.
    value(
        "st_picked",
        format!(
            "if({} = 1, st_answer.{}, CAST([], 'Array(UInt32)'))",
            held(moving::GOING),
            answer::PICKED
        ),
    );
    value(
        "st_pk",
        inter::touch(
            "st_picked",
            &held(moving::PICKED_UP),
            pickups.m_sprite,
            pickups.m_flags,
            pickups.m_z,
            mover.z,
            mover.height,
            pickups.skill,
        ),
    );
    let keep = |field: usize, moved: String| {
        format!("toInt32(if(st_moved = 1, {moved}, {}))", held(field))
    };
    let halved = |field: usize| {
        format!(
            "toInt32(if(st_split = 1, bitShiftRight(toInt64({}), 1), 0))",
            held(field)
        )
    };
    let x = keep(moving::X, "st_tryx".to_owned());
    let y = keep(moving::Y, "st_tryy".to_owned());
    let xmove = halved(moving::XMOVE);
    let ymove = halved(moving::YMOVE);
    let going = format!(
        "toUInt8(st_moved = 1 AND st_split = 1 AND \
         (bitShiftRight(toInt64({}), 1) != 0 OR bitShiftRight(toInt64({}), 1) != 0))",
        held(moving::XMOVE),
        held(moving::YMOVE)
    );
    let floorz = keep(moving::FLOORZ, format!("st_answer.{}", answer::FLOORZ));
    let ceilingz = keep(moving::CEILINGZ, format!("st_answer.{}", answer::CEILINGZ));
    let subsector = keep(
        moving::SUBSECTOR,
        format!("st_answer.{}", answer::SUBSECTOR),
    );
    let blocked = format!(
        "toUInt8({} = 1 OR ({} = 1 AND st_answer.{} = 0))",
        held(moving::BLOCKED),
        held(moving::GOING),
        answer::OK
    );
    let alive = format!(
        "arrayMap((a, k) -> toUInt8(if(has(st_pk.{}, toUInt32(k)), 0, a)), {held_alive}, \
         arrayEnumerate({held_alive}))",
        inter::TAKEN,
        held_alive = held(moving::ALIVE)
    );
    let body = format!(
        "({x}, {y}, {xmove}, {ymove}, {going}, {floorz}, {ceilingz}, {subsector}, \
         {blocked}, st_pk, {alive})"
    );
    let start = format!(
        "(toInt32({x}), toInt32({y}), {xmove}, {ymove}, toUInt8({momx} != 0 OR {momy} != 0), \
         toInt32({floorz}), toInt32({ceilingz}), toInt32({subsector}), toUInt8(0), {pk}, {alive})",
        x = mover.x,
        y = mover.y,
        xmove = clamp(mover.momx),
        ymove = clamp(mover.momy),
        momx = mover.momx,
        momy = mover.momy,
        floorz = mover.floorz,
        ceilingz = mover.ceilingz,
        subsector = mover.subsector,
        pk = pickups.start,
        alive = pickups.alive,
    );
    format!(
        "arrayFold((move_at, move_step) -> {}, range({}), {start})",
        bind::chain(&values, &body),
        steps(mover.momx, mover.momy)
    )
}

/// `P_XYMovement` clamps each axis to `MAXMOVE` before it starts.
fn clamp(mom: &str) -> String {
    format!("toInt32(least(greatest(toInt64({mom}), -{MAXMOVE}), {MAXMOVE}))")
}

/// The friction `P_XYMovement` applies once the move is done.
///
/// A player who is still pressing a key keeps sliding; one who is not and
/// is under `STOPSPEED` stops dead and drops out of the walking frames.
pub fn friction(
    momx: &str,
    momy: &str,
    z: &str,
    floorz: &str,
    forwardmove: &str,
    sidemove: &str,
) -> Vec<(String, String)> {
    let airborne = format!("toInt64({z}) > toInt64({floorz})");
    let stops = format!(
        "{momx} > -{STOPSPEED} AND {momx} < {STOPSPEED} \
         AND {momy} > -{STOPSPEED} AND {momy} < {STOPSPEED} \
         AND {forwardmove} = 0 AND {sidemove} = 0"
    );
    let slowed = |mom: &str| format!("toInt32(bitShiftRight(toInt64({mom}) * {FRICTION}, 16))");
    vec![
        ("mv_airborne".to_owned(), format!("toUInt8({airborne})")),
        ("mv_stops".to_owned(), format!("toUInt8({stops})")),
        (
            "mv_momx".to_owned(),
            format!(
                "toInt32(multiIf(mv_airborne = 1, {momx}, mv_stops = 1, 0, {}))",
                slowed(momx)
            ),
        ),
        (
            "mv_momy".to_owned(),
            format!(
                "toInt32(multiIf(mv_airborne = 1, {momy}, mv_stops = 1, 0, {}))",
                slowed(momy)
            ),
        ),
    ]
}

/// `P_ZMovement` for a thing with no float and no missile: the height
/// moves, the floor and the ceiling clip it, and gravity pulls it down
/// when it is above the floor.
pub fn z_movement(
    z: &str,
    momz: &str,
    floorz: &str,
    ceilingz: &str,
    height: &str,
    flags: &str,
    viewheight: &str,
) -> Vec<(String, String)> {
    vec![
        // A player walking up a step has its view lowered and raised back.
        (
            "mv_stepup".to_owned(),
            format!("toInt64({floorz}) - toInt64({z})"),
        ),
        (
            "mv_viewheight_step".to_owned(),
            format!(
                "toInt32(if(mv_stepup > 0, toInt64({viewheight}) - mv_stepup, toInt64({viewheight})))"
            ),
        ),
        (
            "mv_deltaviewheight_step".to_owned(),
            format!(
                "toInt32(if(mv_stepup > 0, \
                 bitShiftRight({VIEWHEIGHT} - toInt64(mv_viewheight_step), 3), toInt64(0)))"
            ),
        ),
        (
            "mv_zstepped".to_owned(),
            format!("toInt64({z}) + toInt64({momz})"),
        ),
        (
            "mv_onfloor".to_owned(),
            format!("toUInt8(mv_zstepped <= toInt64({floorz}))"),
        ),
        (
            "mv_z_floored".to_owned(),
            format!("if(mv_onfloor = 1, toInt64({floorz}), mv_zstepped)"),
        ),
        (
            "mv_momz_floored".to_owned(),
            format!(
                "toInt32(multiIf(mv_onfloor = 1 AND {momz} < 0, 0, \
                 mv_onfloor = 1, {momz}, \
                 bitAnd({flags}, {MF_NOGRAVITY}) != 0, {momz}, \
                 {momz} = 0, -{}, toInt64({momz}) - {GRAVITY}))",
                GRAVITY * 2
            ),
        ),
        (
            "mv_hitceiling".to_owned(),
            format!("toUInt8(mv_z_floored + toInt64({height}) > toInt64({ceilingz}))"),
        ),
        (
            "mv_z".to_owned(),
            format!(
                "toInt32(if(mv_hitceiling = 1, toInt64({ceilingz}) - toInt64({height}), \
                 mv_z_floored))"
            ),
        ),
        (
            "mv_momz".to_owned(),
            "toInt32(if(mv_hitceiling = 1 AND mv_momz_floored > 0, 0, mv_momz_floored))".to_owned(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn mover() -> Mover<'static> {
        Mover {
            slot: "pl_slot",
            radius: "pl_radius",
            height: "pl_height",
            z: "pl_z",
            flags: "pl_flags",
            is_player: "1",
            momx: "pl_pushx",
            momy: "pl_pushy",
            x: "pl_x",
            y: "pl_y",
            floorz: "pl_floorz",
            ceilingz: "pl_ceilingz",
            subsector: "pl_subsector",
        }
    }

    pub(super) fn world() -> World<'static> {
        World {
            m_x: "w_x",
            m_y: "w_y",
            m_radius: "w_radius",
            m_flags: "w_flags",
            m_linkseq: "w_linkseq",
            alive: "move_at.11",
            floorheight: "w_floor",
            ceilingheight: "w_ceiling",
            line_special: "w_special",
        }
    }

    pub(super) fn pickups() -> Pickups<'static> {
        Pickups {
            m_sprite: "w_sprite",
            m_flags: "w_flags",
            m_z: "w_z",
            skill: "skill",
            start: "pk0",
            alive: "alive0",
        }
    }

    #[test]
    fn the_loop_runs_as_many_steps_as_the_momentum_needs() {
        let text = steps("mx", "my");
        assert!(text.contains("mx = 0 AND my = 0, 0"));
        assert!(text.contains("> 983040 OR"));
        assert!(text.ends_with(", 2, 1))"));
    }

    #[test]
    fn the_loop_is_one_fold() {
        let sql = xy_movement(&mover(), &world(), &pickups());
        assert_eq!(sql.matches("arrayFold((move_at, move_step)").count(), 1);
    }

    #[test]
    fn the_loop_balances_its_parentheses() {
        let sql = xy_movement(&mover(), &world(), &pickups());
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
    }
}
