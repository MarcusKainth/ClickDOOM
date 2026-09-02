//! What a thing does with its momentum, from `p_mobj.c`.

use super::inter;
use super::map::{self, World, answer};
use super::maputl;
use crate::sql::bind;
use crate::sql::fixed;

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
    pub const PHASE: usize = 5;
    pub const FLOORZ: usize = 6;
    pub const CEILINGZ: usize = 7;
    pub const SUBSECTOR: usize = 8;
    pub const HITCOUNT: usize = 9;
    pub const PICKED_UP: usize = 10;
    pub const ALIVE: usize = 11;
    pub const MOMX: usize = 12;
    pub const MOMY: usize = 13;
    pub const SLIDEX: usize = 14;
    pub const SLIDEY: usize = 15;
    pub const USELINE: usize = 16;
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
    /// The angle the command has already turned the thing to.
    pub angle: &'a str,
    /// 1 on the tic the use key goes down.
    pub uses: &'a str,
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

/// Where the loop has got to, which is what the engine's `goto` looks
/// like from outside the function.
mod phase {
    /// `P_XYMovement`'s own loop: try the next part of the move.
    pub const STEP: i64 = 0;
    /// `P_SlideMove`: trace the three leading corners and move up to the
    /// nearest wall any of them hits.
    pub const SLIDE: i64 = 1;
    /// Slide the rest of the way along that wall.
    pub const ALONG: i64 = 2;
    /// `stairstep`: try the two axes on their own.
    pub const STAIR_Y: i64 = 3;
    pub const STAIR_X: i64 = 4;
    /// Nothing left to do.
    pub const DONE: i64 = 5;
    pub const USE: i64 = 6;
}

/// `p_map.c`: how many walls a slide bounces off before it stair-steps.
const SLIDE_TRIES: i64 = 3;
/// `p_map.c`: how far short of the wall the slide stops.
const SLIDE_NUDGE: i64 = 0x800;
/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `tables.h`
const ANG180: i64 = 0x8000_0000;
const ANGLETOFINESHIFT: u32 = 19;
/// `doomdata.h`
const ML_TWOSIDED: i64 = 4;
/// `r_defs.h`
const ST_HORIZONTAL: i64 = 0;
const ST_VERTICAL: i64 = 1;
/// `p_map.c`
const MAXSTEP: i64 = 24 << 16;
/// `p_local.h`: how far in front of itself a thing reaches to use a line,
/// in whole units, which is how `P_UseLines` scales the direction.
const USERANGE: i64 = 64;

/// How many steps past the move itself the loop is given for the slide:
/// a move up to the wall and a move along it for each try, and the two
/// stair steps after them.
const SLIDE_BUDGET: i64 = 2 * SLIDE_TRIES + 2;

/// How many steps the loop is given.
///
/// A move nothing blocks takes one step or two, because halving once puts
/// both axes under half of `MAXMOVE`. What is left is the slide's budget,
/// and a tic that wants more than that says it could not be produced.
pub fn steps(momx: &str, momy: &str, uses: &str) -> String {
    format!(
        "toUInt32(if({uses} = 1, 1, 0) + multiIf({momx} = 0 AND {momy} = 0, 0, \
         {clamped_x} > {half} OR {clamped_y} > {half}, {}, {}))",
        2 + SLIDE_BUDGET,
        1 + SLIDE_BUDGET,
        half = MAXMOVE / 2,
        clamped_x = clamp(momx),
        clamped_y = clamp(momy),
    )
}

/// `P_XYMovement` and `P_SlideMove`, as one fold whose accumulator carries
/// where the thing has got to and which part of the two it is in.
///
/// The steps depend on each other, so the fold is the loop: each reads the
/// position the one before it reached and the world it left, because
/// `P_TouchSpecialThing` takes a thing off the blockmap as it picks it up.
/// `P_TryMove` and `P_PathTraverse` appear once each, and the phase
/// decides what they are asked. A phase that asks nothing passes an empty
/// array, which costs nothing to walk.
pub fn xy_movement(mover: &Mover<'_>, world: &World<'_>, pickups: &Pickups<'_>) -> String {
    let held = |field: usize| format!("move_at.{field}");
    let at = |p: i64| format!("({} = {p})", held(moving::PHASE));
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    let fixed_mul = |a: &str, b: &str| format!("bitShiftRight(toInt64({a}) * toInt64({b}), 16)");

    // The scan: the three leading corners of the box, each traced along
    // the whole of what is left to move.
    let corner = |mom: usize, coord: usize, lead: bool| {
        let sign = if lead { "" } else { "-" };
        format!(
            "toInt64({} {sign}+ if({} > 0, toInt64({r}), -toInt64({r})))",
            held(coord),
            held(mom),
            r = mover.radius
        )
        .replace("-+", "-")
    };
    value("sl_leadx", corner(moving::MOMX, moving::X, true));
    value("sl_leady", corner(moving::MOMY, moving::Y, true));
    value("sl_trailx", corner(moving::MOMX, moving::X, false));
    value("sl_traily", corner(moving::MOMY, moving::Y, false));
    let trace = |x: &str, y: &str| {
        maputl::tracing(
            x,
            y,
            &format!("{x} + {}", held(moving::MOMX)),
            &format!("{y} + {}", held(moving::MOMY)),
        )
    };
    // `P_UseLines` reaches straight ahead from where the thing stands.
    value(
        "sl_fine",
        format!(
            "toUInt32(bitShiftRight(toInt64({}), {ANGLETOFINESHIFT}))",
            mover.angle
        ),
    );
    let reach = |wave: String, coord: usize| {
        format!("toInt64({} + {USERANGE} * toInt64({wave}))", held(coord))
    };
    value(
        "sl_hits",
        maputl::path_traverse(&format!(
            "multiIf({slide}, [{}, {}, {}], {use}, [{}], \
             CAST([], 'Array(Tuple(Int64, Int64, Int64, Int64))'))",
            trace("sl_leadx", "sl_leady"),
            trace("sl_trailx", "sl_leady"),
            trace("sl_leadx", "sl_traily"),
            maputl::tracing(
                &held(moving::X),
                &held(moving::Y),
                &reach(maputl::finecosine("sl_fine"), moving::X),
                &reach(maputl::finesine("sl_fine"), moving::Y),
            ),
            slide = at(phase::SLIDE),
            r#use = at(phase::USE),
        )),
    );
    value(
        "sl_blocking",
        blocking(mover, world, &held, &at(phase::USE)),
    );
    // The nearest wall any of the three traces found. The engine walks them
    // in order and keeps the first of an equal pair, so a stable sort by
    // fraction puts the engine's choice first.
    value(
        "sl_nearest",
        format!(
            "arrayFirst(h -> 1, arrayPushBack(arraySort(h -> h.2, sl_blocking), \
             (toInt32(-1), toInt32({}))))",
            FRACUNIT + 1
        ),
    );
    value("sl_bestfrac", "toInt64(sl_nearest.2)".to_owned());
    value("sl_bestline", "toInt64(sl_nearest.1)".to_owned());
    value(
        "sl_upto",
        format!("toInt64(greatest(sl_bestfrac - {SLIDE_NUDGE}, 0))"),
    );
    value(
        "sl_left",
        format!(
            "toInt64(least({FRACUNIT} - (sl_bestfrac - {SLIDE_NUDGE} + {SLIDE_NUDGE}), {FRACUNIT}))"
        ),
    );
    value("sl_movex", fixed_mul(&held(moving::MOMX), "sl_left"));
    value("sl_movey", fixed_mul(&held(moving::MOMY), "sl_left"));
    for (name, expr) in hit_slide_line("sl_movex", "sl_movey", &held) {
        value(&name, expr);
    }

    // What `P_XYMovement`'s own loop is trying.
    value(
        "st_split",
        format!(
            "toUInt8({} AND ({} > {half} OR {} > {half}))",
            at(phase::STEP),
            held(moving::XMOVE),
            held(moving::YMOVE),
            half = MAXMOVE / 2
        ),
    );
    value(
        "st_dx",
        format!(
            "toInt64(if(st_split = 1, intDiv({}, 2), {}))",
            held(moving::XMOVE),
            held(moving::XMOVE)
        ),
    );
    value(
        "st_dy",
        format!(
            "toInt64(if(st_split = 1, intDiv({}, 2), {}))",
            held(moving::YMOVE),
            held(moving::YMOVE)
        ),
    );
    value(
        "st_tryx",
        format!(
            "toInt32(multiIf({step}, {x} + st_dx, \
             {slide}, {x} + {}, \
             {along}, {x} + {slidex}, \
             {stair_x}, {x} + {momx}, {x}))",
            fixed_mul(&held(moving::MOMX), "sl_upto"),
            x = held(moving::X),
            momx = held(moving::MOMX),
            slidex = held(moving::SLIDEX),
            step = at(phase::STEP),
            slide = at(phase::SLIDE),
            along = at(phase::ALONG),
            stair_x = at(phase::STAIR_X),
        ),
    );
    value(
        "st_tryy",
        format!(
            "toInt32(multiIf({step}, {y} + st_dy, \
             {slide}, {y} + {}, \
             {along}, {y} + {slidey}, \
             {stair_y}, {y} + {momy}, {y}))",
            fixed_mul(&held(moving::MOMY), "sl_upto"),
            y = held(moving::Y),
            momy = held(moving::MOMY),
            slidey = held(moving::SLIDEY),
            step = at(phase::STEP),
            slide = at(phase::SLIDE),
            along = at(phase::ALONG),
            stair_y = at(phase::STAIR_Y),
        ),
    );
    // The scan itself moves the thing only when it found a wall to move up
    // to; a scan that found none goes straight to the stair step.
    value(
        "st_asks",
        format!(
            "toUInt8(NOT {done} AND NOT {use} \
             AND NOT ({slide} AND (sl_bestfrac > {FRACUNIT} OR sl_upto <= 0)))",
            done = at(phase::DONE),
            slide = at(phase::SLIDE),
            r#use = at(phase::USE),
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
        "st_answers",
        map::try_moves(
            &format!(
                "if(st_asks = 1, [{asking}], \
                 CAST([], 'Array(Tuple(UInt32, Int32, Int32, Int32, Int32, Int32, Int32, UInt8))'))"
            ),
            world,
        ),
    );
    value(
        "st_ok",
        format!(
            "toUInt8(st_asks = 1 AND arrayFirst(a -> 1, st_answers).{} = 1)",
            answer::OK
        ),
    );
    value(
        "st_picked",
        format!(
            "if(st_asks = 1, arrayFirst(a -> 1, st_answers).{}, CAST([], 'Array(UInt32)'))",
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
    // Where the loop goes next.
    value(
        "st_next",
        format!(
            "toInt64(multiIf(\
             {done}, {DONE}, \
             {use} AND ({momx} != 0 OR {momy} != 0), {STEP}, \
             {use}, {DONE}, \
             {step} AND st_ok = 1 AND st_split = 1, {STEP}, \
             {step} AND st_ok = 1, {DONE}, \
             {step}, {SLIDE}, \
             {slide} AND sl_bestfrac > {FRACUNIT}, {STAIR_Y}, \
             {slide} AND st_asks = 1 AND st_ok = 0, {STAIR_Y}, \
             {slide} AND sl_left <= 0, {DONE}, \
             {slide}, {ALONG}, \
             {along} AND st_ok = 1, {DONE}, \
             {along} AND {hits} + 1 >= {SLIDE_TRIES}, {STAIR_Y}, \
             {along}, {SLIDE}, \
             {stair_y} AND st_ok = 1, {DONE}, \
             {stair_y}, {STAIR_X}, \
             {DONE}))",
            DONE = phase::DONE,
            STEP = phase::STEP,
            SLIDE = phase::SLIDE,
            ALONG = phase::ALONG,
            STAIR_Y = phase::STAIR_Y,
            STAIR_X = phase::STAIR_X,
            done = at(phase::DONE),
            step = at(phase::STEP),
            slide = at(phase::SLIDE),
            along = at(phase::ALONG),
            stair_y = at(phase::STAIR_Y),
            r#use = at(phase::USE),
            momx = held(moving::MOMX),
            momy = held(moving::MOMY),
            hits = held(moving::HITCOUNT),
        ),
    );
    let keep = |field: usize, when_moved: String| {
        format!("toInt32(if(st_ok = 1, {when_moved}, {}))", held(field))
    };
    let answered = |field: usize| format!("arrayFirst(a -> 1, st_answers).{field}");
    // The accumulator's members, in the order `moving` names them.
    let halved = |field: usize| {
        format!(
            "toInt64(multiIf(NOT {step}, {held}, st_ok = 0, 0, \
             st_split = 1, bitShiftRight({held}, 1), 0))",
            step = at(phase::STEP),
            held = held(field)
        )
    };
    // `P_SlideMove` keeps what it clipped the move down to. The scan is
    // the only phase that traces, so the vector it clipped has to keep
    // until the phase after it tries the move.
    let clipped = |kept: usize, when: &str, from: usize| {
        format!("toInt64(if({when}, {}, {}))", held(from), held(kept))
    };
    let members = [
        keep(moving::X, "st_tryx".to_owned()),
        keep(moving::Y, "st_tryy".to_owned()),
        halved(moving::XMOVE),
        halved(moving::YMOVE),
        "st_next".to_owned(),
        keep(moving::FLOORZ, answered(answer::FLOORZ)),
        keep(moving::CEILINGZ, answered(answer::CEILINGZ)),
        keep(moving::SUBSECTOR, answered(answer::SUBSECTOR)),
        format!(
            "toInt64({} + if({along} AND st_ok = 0, 1, 0))",
            held(moving::HITCOUNT),
            along = at(phase::ALONG)
        ),
        "st_pk".to_owned(),
        format!(
            "arrayMap((a, k) -> toUInt8(if(has(st_pk.{}, toUInt32(k)), 0, a)), {held_alive}, \
             arrayEnumerate({held_alive}))",
            inter::TAKEN,
            held_alive = held(moving::ALIVE)
        ),
        clipped(moving::MOMX, &at(phase::ALONG), moving::SLIDEX),
        clipped(moving::MOMY, &at(phase::ALONG), moving::SLIDEY),
        format!(
            "toInt64(if({slide}, sl_slidex, {}))",
            held(moving::SLIDEX),
            slide = at(phase::SLIDE)
        ),
        format!(
            "toInt64(if({slide}, sl_slidey, {}))",
            held(moving::SLIDEY),
            slide = at(phase::SLIDE)
        ),
        // `PTR_UseTraverse` stops at the first line it cannot see past. A
        // line with a special on it is the one the press acts on; one
        // without is a wall the press does not reach through.
        format!(
            "toInt64(if({use}, if(sl_bestline >= 0 AND {}[1 + sl_bestline] != 0, \
             sl_bestline, -1), {}))",
            world.line_special,
            held(moving::USELINE),
            r#use = at(phase::USE)
        ),
    ];
    let body = format!("({})", members.join(", "));
    let start = format!(
        "(toInt32({x}), toInt32({y}), {xmove}, {ymove}, \
         toInt64(multiIf({uses} = 1, {USE}, {momx} != 0 OR {momy} != 0, {STEP}, {DONE})), \
         toInt32({floorz}), toInt32({ceilingz}), toInt32({subsector}), toInt64(0), {pk}, {alive}, \
         toInt64({xmove}), toInt64({ymove}), toInt64(0), toInt64(0), toInt64(-1))",
        USE = phase::USE,
        STEP = phase::STEP,
        DONE = phase::DONE,
        x = mover.x,
        y = mover.y,
        xmove = format!("toInt64({})", clamp(mover.momx)),
        ymove = format!("toInt64({})", clamp(mover.momy)),
        momx = mover.momx,
        momy = mover.momy,
        floorz = mover.floorz,
        ceilingz = mover.ceilingz,
        subsector = mover.subsector,
        uses = mover.uses,
        pk = pickups.start,
        alive = pickups.alive,
    );
    format!(
        "arrayFold((move_at, move_step) -> {}, range({}), {start})",
        bind::chain(&values, &body),
        steps(mover.momx, mover.momy, mover.uses)
    )
}

/// Whether the loop ran out of steps before it was finished, which is a
/// tic the simulation could not produce in full.
/// The special line `P_UseLines` reached, or -1.
pub fn use_line(loop_state: &str) -> String {
    format!("toInt64({loop_state}.{})", moving::USELINE)
}

pub fn unfinished(loop_state: &str) -> String {
    format!("toUInt8({loop_state}.{} != {})", moving::PHASE, phase::DONE)
}

/// `PTR_SlideTraverse`: the first line of each trace that stops the thing,
/// with the fraction along the trace it stopped at.
fn blocking(
    mover: &Mover<'_>,
    world: &World<'_>,
    held: &dyn Fn(usize) -> String,
    is_use: &str,
) -> String {
    let line = format!("h.{}", maputl::intercept::LINE);
    // The opening is read four times, so it is bound once inside the
    // lambda rather than written out at each of them.
    let stops = bind::chain(
        &[(
            "op".to_owned(),
            maputl::opening(&line, world.floorheight, world.ceilingheight),
        )],
        &format!(
            "if({is_use}, \
             {}[1 + {line}] != 0 OR op.1 - op.2 <= 0, \
             if(bitAnd(line_flags[1 + {line}], {ML_TWOSIDED}) = 0, \
             {} = 0, \
             op.1 - op.2 < toInt64({height}) \
             OR op.1 - toInt64({z}) < toInt64({height}) \
             OR op.2 - toInt64({z}) > {MAXSTEP}))",
            world.line_special,
            map::point_on_line_side(
                &format!("toInt32({})", held(moving::X)),
                &format!("toInt32({})", held(moving::Y)),
                &line
            ),
            height = mover.height,
            z = mover.z,
        ),
    );
    // A trace that reaches nothing ends on the sentinel, which no line
    // can beat and the caller drops.
    format!(
        "arrayFilter(h -> h.2 <= {FRACUNIT}, arrayMap(hits -> \
         arrayFirst(h -> 1, arrayPushBack(arrayFilter(h -> {stops}, hits), \
         (toInt32(-1), toInt32({})))), sl_hits))",
        FRACUNIT + 1
    )
}

/// `P_HitSlideLine`: what is left of the move, turned to run along the
/// wall it hit, as the values that build `sl_slidex` and `sl_slidey`.
///
/// The two axes share every angle and length between them, so each one is
/// a value of its own and the pair reads it.
fn hit_slide_line(
    movex: &str,
    movey: &str,
    held: &dyn Fn(usize) -> String,
) -> Vec<(String, String)> {
    let line = "sl_bestline";
    let slope = format!("line_slopetype[1 + {line}]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value(
        "sl_side",
        map::point_on_line_side(
            &format!("toInt32({})", held(moving::X)),
            &format!("toInt32({})", held(moving::Y)),
            line,
        ),
    );
    value(
        "sl_lineangle",
        format!(
            "toUInt32(bitAnd(toInt64({}) + if(sl_side = 1, {ANG180}, 0), 4294967295))",
            fixed::point_to_angle(
                &format!("line_dx[1 + {line}]"),
                &format!("line_dy[1 + {line}]"),
                "tantoangle"
            )
        ),
    );
    value(
        "sl_moveangle",
        fixed::point_to_angle(
            &format!("toInt32({movex})"),
            &format!("toInt32({movey})"),
            "tantoangle",
        ),
    );
    value(
        "sl_delta",
        "toUInt32(bitAnd(toInt64(sl_moveangle) - toInt64(sl_lineangle) + 4294967296, \
         4294967295))"
            .to_owned(),
    );
    value(
        "sl_deltafine",
        format!(
            "toUInt32(bitShiftRight(bitAnd(if(sl_delta > {ANG180}, \
             toInt64(sl_delta) + {ANG180}, toInt64(sl_delta)), 4294967295), {ANGLETOFINESHIFT}))"
        ),
    );
    value(
        "sl_linefine",
        format!("toUInt32(bitShiftRight(toInt64(sl_lineangle), {ANGLETOFINESHIFT}))"),
    );
    value(
        "sl_movelen",
        fixed::aprox_distance(&format!("toInt32({movex})"), &format!("toInt32({movey})")),
    );
    value(
        "sl_newlen",
        fixed::fixed_mul("toInt32(sl_movelen)", &maputl::finecosine("sl_deltafine")),
    );
    // A wall square to an axis keeps the move on that axis; every other
    // wall takes the length turned to run along it.
    let axis = |along: String, flat: &str, upright: &str, whole: &str| {
        format!(
            "toInt64(multiIf({line} < 0, toInt64({whole}), \
             {slope} = {ST_HORIZONTAL}, toInt64({flat}), \
             {slope} = {ST_VERTICAL}, toInt64({upright}), \
             toInt64({along})))"
        )
    };
    value(
        "sl_slidex",
        axis(
            fixed::fixed_mul("toInt32(sl_newlen)", &maputl::finecosine("sl_linefine")),
            movex,
            "0",
            movex,
        ),
    );
    value(
        "sl_slidey",
        axis(
            fixed::fixed_mul("toInt32(sl_newlen)", &maputl::finesine("sl_linefine")),
            "0",
            movey,
            movey,
        ),
    );
    values
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
            angle: "pl_angle",
            uses: "pl_uses",
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
        let text = steps("mx", "my", "u");
        assert!(text.contains("mx = 0 AND my = 0, 0"));
        assert!(text.contains("> 983040 OR"));
        assert!(text.contains("if(u = 1, 1, 0) +"), "{text}");
        assert!(text.ends_with(", 10, 9))"), "{text}");
    }

    /// `P_UseLines` reads the blockmap the same way `P_SlideMove` does, so
    /// the two share the one traverser the fold holds.
    #[test]
    fn the_loop_traverses_in_one_place() {
        let sql = xy_movement(&mover(), &world(), &pickups());
        assert_eq!(sql.matches("arrayFold((w, s)").count(), 1, "{sql}");
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
