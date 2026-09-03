//! The hitscan attacks, from `p_map.c`.
//!
//! `P_AimLineAttack` traces out of a thing at its own angle and answers the
//! slope a shot has to leave at to reach whatever the thing is pointing at.
//! `PTR_AimTraverse` narrows the slopes a target may sit between as the
//! trace crosses each two-sided line, and takes the first thing that sits
//! inside them.
//!
//! The traverser is a fold over the intercepts `P_PathTraverse` hands over,
//! because each one reads the slopes the ones before it left and the walk
//! stops on the first that closes the view. A tic that asks for no trace
//! walks nothing.

use crate::sql::{bind, fixed};

use super::maputl::{self, intercept};

/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
/// `doomdata.h`
const ML_TWOSIDED: i64 = 4;
/// `tables.h`
const ANGLETOFINESHIFT: u32 = 19;
/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `p_map.c`: how far above and below itself a thing looks, which is the
/// view window's own 100 rows over 160 columns.
const TOPSLOPE: i64 = 100 * FRACUNIT / 160;
/// `p_pspr.c`: how far `P_BulletSlope` looks, and the swing either side of
/// the thing's own angle it tries when the aim straight ahead finds
/// nothing.
const AIMRANGE: i64 = 16 * 64 * FRACUNIT;
const AIMSWING: i64 = 1 << 26;
/// `angle_t` wraps at 32 bits.
const ANGLE_WRAP: i64 = 1 << 32;

/// Where each field of an aim ask sits in its tuple.
pub mod ask {
    /// The mobj slot doing the shooting, which its own trace cannot hit.
    pub const SHOOTER: usize = 1;
    pub const X: usize = 2;
    pub const Y: usize = 3;
    pub const Z: usize = 4;
    pub const HEIGHT: usize = 5;
    pub const ANGLE: usize = 6;
    pub const RANGE: usize = 7;
    /// The slope the shot leaves at. Only a shot's own ask carries it; an
    /// aim works one out.
    pub const SLOPE: usize = 8;
}

/// Where each field of an aim answer sits in its tuple.
pub mod answer {
    pub const SLOPE: usize = 1;
    /// The mobj slot aimed at, 0 for none.
    pub const TARGET: usize = 2;
}

/// Where each field of a shot's answer sits in its tuple.
pub mod hit {
    /// 0 the shot reached nothing it may spawn on, 1 a line, 2 a thing.
    pub const KIND: usize = 1;
    /// The line, or the mobj slot when [`KIND`] is 2.
    pub const ID: usize = 2;
    pub const X: usize = 3;
    pub const Y: usize = 4;
    pub const Z: usize = 5;
    /// The lines carrying a special the shot crossed, in the order it
    /// crossed them.
    pub const SPECHIT: usize = 6;
}

/// Where each field of the aim traverser's accumulator sits.
mod held {
    pub const TOPSLOPE: usize = 1;
    pub const BOTTOMSLOPE: usize = 2;
    pub const SLOPE: usize = 3;
    pub const TARGET: usize = 4;
    pub const STOPPED: usize = 5;
}

/// Where the shot traverser's accumulator carries whether the walk has
/// stopped. Everything before it is the answer, in [`hit`]'s order.
mod reached {
    pub const STOPPED: usize = 7;
}

/// One aim, as the tuple [`aim_line_attack`] reads.
pub fn asking(
    shooter: &str,
    x: &str,
    y: &str,
    z: &str,
    height: &str,
    angle: &str,
    range: &str,
) -> String {
    format!(
        "(toUInt32({shooter}), toInt32({x}), toInt32({y}), toInt32({z}), \
         toInt32({height}), toUInt32({angle}), toInt32({range}))"
    )
}

/// One shot, as the tuple [`line_attack`] reads. The slope is what
/// [`bullet_slope`] worked out.
#[allow(clippy::too_many_arguments)]
pub fn shooting(
    shooter: &str,
    x: &str,
    y: &str,
    z: &str,
    height: &str,
    angle: &str,
    range: &str,
    slope: &str,
) -> String {
    format!(
        "(toUInt32({shooter}), toInt32({x}), toInt32({y}), toInt32({z}), \
         toInt32({height}), toUInt32({angle}), toInt32({range}), toInt32({slope}))"
    )
}

/// The engine tables a shot reads that no other stage does: each sector's
/// ceiling flat, and the sky's own flat number.
pub fn constants(db: &str) -> Vec<(String, String)> {
    vec![
        (
            "sec_ceilingpic".to_owned(),
            super::table_column(db, "lv_sectors_static", "ceilingpic"),
        ),
        (
            "skyflatnum".to_owned(),
            format!(
                "assumeNotNull((SELECT toInt32(id) FROM {db}.flats WHERE upper(name) = 'F_SKY1'))"
            ),
        ),
    ]
}

/// The arrays a trace reads: where every mobj stands, how high each sector
/// is and what special each line carries at this point in the tic.
pub struct Targets<'a> {
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_z: &'a str,
    pub m_radius: &'a str,
    pub m_height: &'a str,
    pub m_flags: &'a str,
    pub m_linkseq: &'a str,
    /// One per mobj slot: 1 while it is still on the list.
    pub alive: &'a str,
    pub floorheight: &'a str,
    pub ceilingheight: &'a str,
    pub line_special: &'a str,
}

impl Targets<'_> {
    /// The mobjs `PT_ADDTHINGS` walks.
    fn things(&self) -> maputl::Things<'_> {
        maputl::Things {
            m_x: self.m_x,
            m_y: self.m_y,
            m_radius: self.m_radius,
            m_linkseq: self.m_linkseq,
            alive: self.alive,
        }
    }
}

/// `P_AimLineAttack` over every ask in `asks`, as `(slope, target)` each.
///
/// The slope is what a shot leaving the thing has to take to reach the
/// target, and 0 where the trace found none.
///
/// `asks` is named twice, so a caller binds it rather than writing the
/// list out.
pub fn aim_line_attack(asks: &str, world: &Targets<'_>) -> String {
    over_traces(asks, world, &aim_traverse(&|field| ask_of(field), world))
}

/// `P_LineAttack` over every ask in `asks`, as what the shot reached.
///
/// The answer is `(kind, id, x, y, z, spechit)`: kind 1 where the shot
/// ended on a line and 2 where it ended on a thing, with the point the
/// puff or the blood goes at, and the special lines it crossed on the way.
/// A shot that reached nothing, and one that ended on a line showing the
/// sky, answer kind 0 and spawn nothing.
///
/// `asks` is named twice, so a caller binds it rather than writing the
/// list out.
pub fn line_attack(asks: &str, world: &Targets<'_>) -> String {
    over_traces(asks, world, &shoot_traverse(&ask_of, world))
}

/// The ask each traverser reads, as the lambda parameter [`over_traces`]
/// gives it.
fn ask_of(field: usize) -> String {
    format!("am_ask.{field}")
}

/// `P_PathTraverse` over the trace each ask names, with what `traverser`
/// makes of the answer zipped back to the ask that asked for it.
fn over_traces(asks: &str, world: &Targets<'_>, traverser: &str) -> String {
    let a = |field: usize| ask_of(field);
    let traces = format!(
        "arrayMap(am_ask -> {}, {asks})",
        bind::chain_in(
            "amr",
            &ends(&a),
            &maputl::tracing(&a(ask::X), &a(ask::Y), "sh_x2", "sh_y2"),
        )
    );
    format!(
        "arrayMap((am_ask, am_hits) -> {traverser}, {asks}, {})",
        maputl::path_traverse(&traces, Some(&world.things()))
    )
}

/// `P_BulletSlope` over every ask in `shooters`, as `(slope, target)` each.
///
/// The engine aims straight ahead, then a swing to each side, and stops at
/// the first try that finds a target. A try that finds none answers 0 and
/// moves nothing, so the three are asked together and the answer is the
/// first that found something, or the last try.
pub fn bullet_slope(shooters: &str, world: &Targets<'_>) -> String {
    let s = |field: usize| format!("bs_ask.{field}");
    let angle = s(ask::ANGLE);
    let swung = |by: String| {
        asking(
            &s(ask::SHOOTER),
            &s(ask::X),
            &s(ask::Y),
            &s(ask::Z),
            &s(ask::HEIGHT),
            &by,
            &AIMRANGE.to_string(),
        )
    };
    let turned = |by: i64| {
        format!(
            "toUInt32(bitAnd(toUInt64({angle}) + {}, {}))",
            by.rem_euclid(ANGLE_WRAP),
            ANGLE_WRAP - 1
        )
    };
    let tries = format!(
        "arrayFlatten(arrayMap(bs_ask -> [{}, {}, {}], {shooters}))",
        swung(angle.clone()),
        swung(turned(AIMSWING)),
        swung(turned(-AIMSWING)),
    );
    // The tries come back flattened, so each try's own answers are the
    // ones three apart. Reading them as three arrays keeps the pick's
    // lambda on its own parameters.
    let nth = |of: usize| {
        format!(
            "arrayFilter((v, k) -> modulo(k, 3) = {}, bs_aimed, arrayEnumerate(bs_aimed))",
            of % 3
        )
    };
    let body = format!(
        "arrayMap((bs_ahead, bs_left, bs_right) -> \
         multiIf(bs_ahead.{target} != 0, bs_ahead, bs_left.{target} != 0, bs_left, bs_right), \
         {}, {}, {})",
        nth(1),
        nth(2),
        nth(3),
        target = answer::TARGET,
    );
    bind::chain_in(
        "bs",
        &[
            ("bs_tries".to_owned(), tries),
            ("bs_aimed".to_owned(), aim_line_attack("bs_tries", world)),
        ],
        &body,
    )
}

/// Where the trace ends: `distance` map units along the thing's own angle.
fn ends(ask: &dyn Fn(usize) -> String) -> Vec<(String, String)> {
    let along = |wave: String, coord: usize| {
        format!(
            "toInt32(toInt64({}) + toInt64(toInt32(sh_reach * toInt64({wave}))))",
            ask(coord)
        )
    };
    vec![
        (
            "sh_fine".to_owned(),
            format!(
                "toUInt32(bitShiftRight(toUInt32({}), {ANGLETOFINESHIFT}))",
                ask(ask::ANGLE)
            ),
        ),
        (
            "sh_reach".to_owned(),
            format!("toInt64(bitShiftRight(toInt64({}), 16))", ask(ask::RANGE)),
        ),
        (
            "sh_x2".to_owned(),
            along(maputl::finecosine("sh_fine"), ask::X),
        ),
        (
            "sh_y2".to_owned(),
            along(maputl::finesine("sh_fine"), ask::Y),
        ),
    ]
}

/// The height a shot leaves from: half the thing's own, plus eight units.
fn shoot_z(ask: &dyn Fn(usize) -> String) -> String {
    format!(
        "toInt32(toInt64({}) + bitShiftRight(toInt64({}), 1) + {})",
        ask(ask::Z),
        ask(ask::HEIGHT),
        8 * FRACUNIT
    )
}

/// `PTR_ShootTraverse` over `am_hits`, the intercepts of one trace.
///
/// The shot crosses a two-sided line while the slope it left at still fits
/// through the opening, and ends on the first line or thing that stops it.
fn shoot_traverse(ask: &dyn Fn(usize) -> String, world: &Targets<'_>) -> String {
    let at = |field: usize| format!("am_at.{field}");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    let slope = ask(ask::SLOPE);

    value("sh_id", format!("toInt32(am_in.{})", intercept::ID));
    value(
        "sh_dist",
        fixed::fixed_mul(&ask(ask::RANGE), &format!("am_in.{}", intercept::FRAC)),
    );
    value(
        "sh_open",
        maputl::opening("sh_id", world.floorheight, world.ceilingheight),
    );
    value(
        "sh_backless",
        "toUInt8(line_back[1 + sh_id] = -1)".to_owned(),
    );
    let steps = |height: &str| {
        format!(
            "toUInt8(sh_backless = 1 OR {height}[1 + line_front[1 + sh_id]] \
             != {height}[1 + line_back[1 + sh_id]])"
        )
    };
    value("sh_floor_steps", steps(world.floorheight));
    value("sh_ceiling_steps", steps(world.ceilingheight));
    let window = |edge: usize| {
        fixed::fixed_div(
            &format!("toInt32(toInt64(sh_open.{edge}) - toInt64(sh_shootz))"),
            "sh_dist",
        )
    };
    // A line stops the shot where it is not two-sided, or where the slope
    // passes outside the opening on a side whose own height steps.
    value(
        "sh_line_stops",
        format!(
            "toUInt8(bitAnd(line_flags[1 + sh_id], {ML_TWOSIDED}) = 0 \
             OR (sh_floor_steps = 1 AND {} > {slope}) \
             OR (sh_ceiling_steps = 1 AND {} < {slope}))",
            window(2),
            window(1),
        ),
    );
    // A thing stops the shot where the slope passes between its own top and
    // bottom.
    let field = |array: &str| format!("{array}[sh_id]");
    let slope_to = |height: String| {
        fixed::fixed_div(
            &format!("toInt32({height} - toInt64(sh_shootz))"),
            "sh_dist",
        )
    };
    value(
        "sh_thing_stops",
        format!(
            "toUInt8(sh_id != {shooter} AND bitAnd({flags}, {MF_SHOOTABLE}) != 0 \
             AND {top} >= {slope} AND {bottom} <= {slope})",
            shooter = ask(ask::SHOOTER),
            flags = field(world.m_flags),
            top = slope_to(format!(
                "toInt64({}) + toInt64({})",
                field(world.m_z),
                field(world.m_height)
            )),
            bottom = slope_to(format!("toInt64({})", field(world.m_z))),
        ),
    );
    // What is spawned sits a little short of what stopped the shot: four
    // units of the range for a line and ten for a thing.
    value(
        "sh_frac",
        format!(
            "toInt32(toInt64(am_in.{frac}) - toInt64(if(am_in.{is_line} = 1, {}, {})))",
            fixed::fixed_div(&(4 * FRACUNIT).to_string(), &ask(ask::RANGE)),
            fixed::fixed_div(&(10 * FRACUNIT).to_string(), &ask(ask::RANGE)),
            frac = intercept::FRAC,
            is_line = intercept::IS_LINE,
        ),
    );
    let along = |start: &str, end: &str| {
        format!(
            "toInt32(toInt64({start}) + toInt64({}))",
            fixed::fixed_mul(
                &format!("toInt32(toInt64({end}) - toInt64({start}))"),
                "sh_frac"
            )
        )
    };
    value("sh_x", along("sh_tx", "sh_x2"));
    value("sh_y", along("sh_ty", "sh_y2"));
    value(
        "sh_z",
        format!(
            "toInt32(toInt64(sh_shootz) + toInt64({}))",
            fixed::fixed_mul(&slope, &fixed::fixed_mul("sh_frac", &ask(ask::RANGE)))
        ),
    );
    // The sky is not shot at. A line whose front ceiling shows it stops the
    // shot and spawns nothing, either because the point is above that
    // ceiling or because the sector behind shows the sky too.
    value(
        "sh_sky",
        format!(
            "toUInt8(sec_ceilingpic[1 + line_front[1 + sh_id]] = skyflatnum \
             AND (sh_z > {ceiling}[1 + line_front[1 + sh_id]] \
             OR (sh_backless = 0 \
             AND sec_ceilingpic[1 + line_back[1 + sh_id]] = skyflatnum)))",
            ceiling = world.ceilingheight,
        ),
    );
    // `P_ShootSpecialLine` runs for every line the shot crosses, the one it
    // ends on included.
    value(
        "sh_spechit",
        format!(
            "if(am_in.{is_line} = 1 AND {special}[1 + sh_id] != 0, \
             arrayPushBack({held}, sh_id), {held})",
            is_line = intercept::IS_LINE,
            special = world.line_special,
            held = at(hit::SPECHIT),
        ),
    );

    let carrying = |kind: &str, id: &str, x: &str, y: &str, z: &str, stopped: u8| {
        format!(
            "(toUInt8({kind}), toInt32({id}), toInt32({x}), toInt32({y}), \
             toInt32({z}), sh_spechit, toUInt8({stopped}))"
        )
    };
    let carries_on = carrying(
        &at(hit::KIND),
        &at(hit::ID),
        &at(hit::X),
        &at(hit::Y),
        &at(hit::Z),
        0,
    );
    let stops_on_sky = carrying(
        &at(hit::KIND),
        &at(hit::ID),
        &at(hit::X),
        &at(hit::Y),
        &at(hit::Z),
        1,
    );
    let ends_on_line = carrying("1", "sh_id", "sh_x", "sh_y", "sh_z", 1);
    let ends_on_thing = carrying("2", "sh_id", "sh_x", "sh_y", "sh_z", 1);
    let body = format!(
        "multiIf({stopped} = 1, am_at, \
         am_in.{is_line} = 1, multiIf(sh_line_stops = 0, {carries_on}, \
         sh_sky = 1, {stops_on_sky}, {ends_on_line}), \
         sh_thing_stops = 1, {ends_on_thing}, {carries_on})",
        stopped = at(reached::STOPPED),
        is_line = intercept::IS_LINE,
    );
    let ran = format!(
        "arrayFold((am_at, am_in) -> {}, am_hits, \
         (toUInt8(0), toInt32(0), toInt32(0), toInt32(0), toInt32(0), \
         CAST([], 'Array(Int32)'), toUInt8(0)))",
        bind::chain_in("sha", &values, &body),
    );
    let answer = bind::chain_in(
        "shv",
        &[("sh_ran".to_owned(), ran)],
        &format!(
            "(sh_ran.{}, sh_ran.{}, sh_ran.{}, sh_ran.{}, sh_ran.{}, sh_ran.{})",
            hit::KIND,
            hit::ID,
            hit::X,
            hit::Y,
            hit::Z,
            hit::SPECHIT,
        ),
    );
    // The trace's own start and end and the height the shot leaves from
    // belong to the ask rather than to the intercept, so they sit outside
    // the fold.
    let mut trace = ends(ask);
    trace.push((
        "sh_tx".to_owned(),
        maputl::nudged(&ask(ask::X), "bmap_orgx"),
    ));
    trace.push((
        "sh_ty".to_owned(),
        maputl::nudged(&ask(ask::Y), "bmap_orgy"),
    ));
    trace.push(("sh_shootz".to_owned(), shoot_z(ask)));
    bind::chain_in("sho", &trace, &answer)
}

/// `PTR_AimTraverse` over `am_hits`, the intercepts of one trace.
fn aim_traverse(ask: &dyn Fn(usize) -> String, world: &Targets<'_>) -> String {
    let at = |field: usize| format!("am_at.{field}");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("am_id", format!("toInt32(am_in.{})", intercept::ID));
    value(
        "am_dist",
        fixed::fixed_mul(&ask(ask::RANGE), &format!("am_in.{}", intercept::FRAC)),
    );
    // A two-sided line narrows the window a target may sit in; anything
    // else stops the trace.
    value(
        "am_open",
        maputl::opening("am_id", world.floorheight, world.ceilingheight),
    );
    let sector = |side: &str| format!("[1 + line_{side}[1 + am_id]]");
    // A line flagged two-sided with no second side leaves the engine's own
    // opening at whatever the last line put there. `noise::guards` stops
    // the load on a map that holds one.
    value(
        "am_backless",
        "toUInt8(line_back[1 + am_id] = -1)".to_owned(),
    );
    let steps = |height: &str| {
        format!(
            "toUInt8(am_backless = 1 OR {height}{front} != {height}{back})",
            front = sector("front"),
            back = sector("back"),
        )
    };
    value("am_floor_steps", steps(world.floorheight));
    value("am_ceiling_steps", steps(world.ceilingheight));
    let window = |edge: usize| {
        fixed::fixed_div(
            &format!("toInt32(toInt64(am_open.{edge}) - toInt64(am_shootz))"),
            "am_dist",
        )
    };
    value(
        "am_bottom",
        format!(
            "toInt32(if(am_floor_steps = 1, greatest({held}, {}), {held}))",
            window(2),
            held = at(held::BOTTOMSLOPE),
        ),
    );
    value(
        "am_top",
        format!(
            "toInt32(if(am_ceiling_steps = 1, least({held}, {}), {held}))",
            window(1),
            held = at(held::TOPSLOPE),
        ),
    );
    value(
        "am_line_stops",
        format!(
            "toUInt8(bitAnd(line_flags[1 + am_id], {ML_TWOSIDED}) = 0 \
             OR am_open.2 >= am_open.1 OR am_top <= am_bottom)"
        ),
    );
    // A thing whose own top and bottom fall inside the window is what the
    // aim was looking for.
    let field = |array: &str| format!("{array}[am_id]");
    let slope_to = |height: String| {
        fixed::fixed_div(
            &format!("toInt32({height} - toInt64(am_shootz))"),
            "am_dist",
        )
    };
    value(
        "am_thing_top",
        slope_to(format!(
            "toInt64({}) + toInt64({})",
            field(world.m_z),
            field(world.m_height)
        )),
    );
    value(
        "am_thing_bottom",
        slope_to(format!("toInt64({})", field(world.m_z))),
    );
    value(
        "am_aimed",
        format!(
            "toUInt8(am_id != {shooter} AND bitAnd({flags}, {MF_SHOOTABLE}) != 0 \
             AND am_thing_top >= {bottom} AND am_thing_bottom <= {top})",
            shooter = ask(ask::SHOOTER),
            flags = field(world.m_flags),
            bottom = at(held::BOTTOMSLOPE),
            top = at(held::TOPSLOPE),
        ),
    );
    // `(thingtopslope + thingbottomslope) / 2`, each held inside the
    // window the lines left.
    value(
        "am_slope",
        format!(
            "toInt32(intDiv(toInt64(least(am_thing_top, {top})) \
             + toInt64(greatest(am_thing_bottom, {bottom})), 2))",
            top = at(held::TOPSLOPE),
            bottom = at(held::BOTTOMSLOPE),
        ),
    );

    let carrying = |top: String, bottom: String, slope: String, target: String, stopped: u8| {
        format!("({top}, {bottom}, {slope}, {target}, toUInt8({stopped}))")
    };
    let stops = carrying(
        at(held::TOPSLOPE),
        at(held::BOTTOMSLOPE),
        at(held::SLOPE),
        at(held::TARGET),
        1,
    );
    let crosses = carrying(
        "am_top".to_owned(),
        "am_bottom".to_owned(),
        at(held::SLOPE),
        at(held::TARGET),
        0,
    );
    let takes = carrying(
        at(held::TOPSLOPE),
        at(held::BOTTOMSLOPE),
        "am_slope".to_owned(),
        "am_id".to_owned(),
        1,
    );
    let body = format!(
        "multiIf({stopped} = 1, am_at, \
         am_in.{is_line} = 1, if(am_line_stops = 1, {stops}, {crosses}), \
         am_aimed = 1, {takes}, am_at)",
        stopped = at(held::STOPPED),
        is_line = intercept::IS_LINE,
    );
    let ran = format!(
        "arrayFold((am_at, am_in) -> {}, am_hits, \
         (toInt32({TOPSLOPE}), toInt32({}), toInt32(0), toInt32(0), toUInt8(0)))",
        bind::chain_in("ama", &values, &body),
        -TOPSLOPE,
    );
    let answer = bind::chain_in(
        "amv",
        &[("am_ran".to_owned(), ran)],
        &format!(
            "(toInt32(am_ran.{}), toInt32(am_ran.{}))",
            held::SLOPE,
            held::TARGET
        ),
    );
    // The height the aim looks from is the ask's, not the intercept's, so
    // it sits outside the fold.
    bind::chain_in("amo", &[("am_shootz".to_owned(), shoot_z(ask))], &answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> Targets<'static> {
        Targets {
            m_x: "m_x",
            m_y: "m_y",
            m_z: "m_z",
            m_radius: "m_radius",
            m_height: "m_height",
            m_flags: "m_flags",
            m_linkseq: "m_linkseq",
            alive: "alive",
            floorheight: "floorheight",
            ceilingheight: "ceilingheight",
            line_special: "line_special",
        }
    }

    /// A traverser walks what one trace crossed, so a trace that crossed
    /// nothing runs no step and a tic that asks for no trace runs no fold.
    #[test]
    fn each_traverser_is_one_fold_over_the_intercepts() {
        for sql in [
            aim_line_attack("asks", &world()),
            line_attack("asks", &world()),
        ] {
            assert_eq!(sql.matches("arrayFold((am_at, am_in)").count(), 1);
            assert_eq!(sql.matches("arrayMap((am_ask, am_hits)").count(), 1);
        }
    }

    /// A fold body that reads neither of its parameters is evaluated
    /// outside the fold whatever the fold does.
    #[test]
    fn each_fold_body_reads_the_intercept_it_is_given() {
        for sql in [
            aim_line_attack("asks", &world()),
            line_attack("asks", &world()),
        ] {
            let (_, body) = sql.split_once("arrayFold((am_at, am_in) -> ").unwrap();
            let (body, _) = body.split_once(", am_hits,").unwrap();
            assert!(body.contains("am_in."), "{body}");
        }
    }

    /// The shot reads the slope it was given rather than working one out.
    #[test]
    fn the_shot_reads_the_slope_it_was_asked_with() {
        let sql = line_attack("asks", &world());
        assert!(sql.contains(&format!("am_ask.{}", ask::SLOPE)), "{sql}");
        assert!(!aim_line_attack("asks", &world()).contains(&format!("am_ask.{}", ask::SLOPE)));
    }

    /// `P_BulletSlope` asks three angles and one aim.
    #[test]
    fn the_bullet_slope_asks_three_angles() {
        let sql = bullet_slope("shooters", &world());
        assert_eq!(sql.matches("arrayFold((am_at, am_in)").count(), 1);
        assert_eq!(sql.matches("toUInt32(bs_ask.6)").count(), 1);
        assert_eq!(sql.matches(&format!("+ {AIMSWING}, 4294967295")).count(), 1);
        assert_eq!(
            sql.matches(&format!("+ {}, 4294967295", ANGLE_WRAP - AIMSWING))
                .count(),
            1
        );
    }

    #[test]
    fn every_expression_balances_its_parentheses() {
        for sql in [
            aim_line_attack("asks", &world()),
            line_attack("asks", &world()),
            bullet_slope("shooters", &world()),
            asking("s", "x", "y", "z", "h", "a", "r"),
            shooting("s", "x", "y", "z", "h", "a", "r", "sl"),
        ] {
            let depth = sql.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{sql}");
        }
    }
}
