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
pub const AIMRANGE: i64 = 16 * 64 * FRACUNIT;
pub const AIMSWING: i64 = 1 << 26;
/// `angle_t` wraps at 32 bits.
pub const ANGLE_WRAP: i64 = 1 << 32;

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
    /// 1 where the ask is an aim, 0 where it is a shot. The walk is the
    /// same either way and only the traverser differs, so both go down one
    /// expression and this is what picks between them.
    pub const AIMS: usize = 9;
}

/// Where each field of a traverse answer sits in its tuple. An aim reads
/// the first two and a shot the rest.
pub mod reached {
    /// The slope the aim found, 0 where it found no target.
    pub const SLOPE: usize = 1;
    /// The mobj slot the aim found, 0 for none.
    pub const TARGET: usize = 2;
    /// 0 the shot reached nothing it may spawn on, 1 a line, 2 a thing.
    pub const KIND: usize = 3;
    /// The line, or the mobj slot when [`KIND`] is 2.
    pub const ID: usize = 4;
    pub const X: usize = 5;
    pub const Y: usize = 6;
    pub const Z: usize = 7;
    /// The lines carrying a special the shot crossed, in the order it
    /// crossed them.
    pub const SPECHIT: usize = 8;
    /// Whether the walk has stopped. The accumulator carries it and the
    /// answer drops it, along with the window an aim narrows as it goes.
    pub(super) const STOPPED: usize = 9;
    pub(super) const TOPSLOPE: usize = 10;
    pub(super) const BOTTOMSLOPE: usize = 11;
    /// How many fields the answer keeps.
    pub const WIDTH: usize = 8;
}

/// One aim, as the tuple [`traverse`] reads. An aim works its own slope
/// out, so the one it carries is never read.
pub fn asking(
    shooter: &str,
    x: &str,
    y: &str,
    z: &str,
    height: &str,
    angle: &str,
    range: &str,
) -> String {
    tuple(shooter, x, y, z, height, angle, range, "0", "1")
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
    tuple(shooter, x, y, z, height, angle, range, slope, "0")
}

/// One ask, whichever of the two it is.
#[allow(clippy::too_many_arguments)]
fn tuple(
    shooter: &str,
    x: &str,
    y: &str,
    z: &str,
    height: &str,
    angle: &str,
    range: &str,
    slope: &str,
    aims: &str,
) -> String {
    format!(
        "(toUInt32({shooter}), toInt32({x}), toInt32({y}), toInt32({z}), \
         toInt32({height}), toUInt32({angle}), toInt32({range}), toInt32({slope}), \
         toUInt8({aims}))"
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

/// `P_AimLineAttack` and `P_LineAttack` over every ask in `asks`.
///
/// One walk serves both. `P_PathTraverse` is the same for an aim and for a
/// shot, and only what folds over the intercepts differs, so the blockmap
/// walk, the intercept fractions and their order appear once and the ask's
/// own flag picks the traverser.
///
/// The answer is `(slope, target, kind, id, x, y, z, spechit)`: an aim
/// reads the first two and a shot the rest.
///
/// `asks` is named twice, so a caller binds it rather than writing the
/// list out.
pub fn traverse(asks: &str, world: &Targets<'_>) -> String {
    let a = ask_of;
    let traces = format!(
        "arrayMap(am_ask -> {}, {asks})",
        bind::chain_in(
            "amr",
            &ends(&a),
            &maputl::tracing(&a(ask::X), &a(ask::Y), "sh_x2", "sh_y2"),
        )
    );
    format!(
        "arrayMap((am_ask, am_hits) -> {}, {asks}, {})",
        traverser(&a, world),
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
        target = reached::TARGET,
    );
    bind::chain_in(
        "bs",
        &[
            ("bs_tries".to_owned(), tries),
            ("bs_aimed".to_owned(), traverse("bs_tries", world)),
        ],
        &body,
    )
}

/// The ask each traverser reads, as the lambda parameter [`traverse`]
/// gives it.
fn ask_of(field: usize) -> String {
    format!("am_ask.{field}")
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

/// `PTR_AimTraverse` and `PTR_ShootTraverse` over `am_hits`, the intercepts
/// of one trace.
///
/// The two ask the same questions of a line and of a thing and differ in
/// what they do with the answers, so the distance, the opening, the sides
/// that step and the thing's own two slopes are worked out once and each
/// arm reads them.
fn traverser(ask: &dyn Fn(usize) -> String, world: &Targets<'_>) -> String {
    let at = |field: usize| format!("am_at.{field}");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    let slope = ask(ask::SLOPE);

    value("am_id", format!("toInt32(am_in.{})", intercept::ID));
    value(
        "am_dist",
        fixed::fixed_mul(&ask(ask::RANGE), &format!("am_in.{}", intercept::FRAC)),
    );
    value(
        "am_open",
        maputl::opening("am_id", world.floorheight, world.ceilingheight),
    );
    // A line flagged two-sided with no second side leaves the engine's own
    // opening at whatever the last line put there. `noise::guards` stops
    // the load on a map that holds one.
    value(
        "am_backless",
        "toUInt8(line_back[1 + am_id] = -1)".to_owned(),
    );
    let steps = |height: &str| {
        format!(
            "toUInt8(am_backless = 1 OR {height}[1 + line_front[1 + am_id]] \
             != {height}[1 + line_back[1 + am_id]])"
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
    value("am_open_bottom", window(2));
    value("am_open_top", window(1));
    value(
        "am_two_sided",
        format!("toUInt8(bitAnd(line_flags[1 + am_id], {ML_TWOSIDED}) != 0)"),
    );

    // The aim's window, narrowed by the sides that step.
    value(
        "am_bottom",
        format!(
            "toInt32(if(am_floor_steps = 1, greatest({held}, am_open_bottom), {held}))",
            held = at(reached::BOTTOMSLOPE),
        ),
    );
    value(
        "am_top",
        format!(
            "toInt32(if(am_ceiling_steps = 1, least({held}, am_open_top), {held}))",
            held = at(reached::TOPSLOPE),
        ),
    );
    value(
        "am_line_stops",
        "toUInt8(am_two_sided = 0 OR am_open.2 >= am_open.1 OR am_top <= am_bottom)".to_owned(),
    );
    // The shot's own line test: it carries on while the slope it left at
    // still fits through the opening.
    value(
        "am_line_blocks",
        format!(
            "toUInt8(am_two_sided = 0 \
             OR (am_floor_steps = 1 AND am_open_bottom > {slope}) \
             OR (am_ceiling_steps = 1 AND am_open_top < {slope}))"
        ),
    );

    // The thing, and the two slopes both arms measure to it.
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
        "am_shootable",
        format!(
            "toUInt8(am_id != {shooter} AND bitAnd({flags}, {MF_SHOOTABLE}) != 0)",
            shooter = ask(ask::SHOOTER),
            flags = field(world.m_flags),
        ),
    );
    value(
        "am_aimed",
        format!(
            "toUInt8(am_shootable = 1 AND am_thing_top >= {bottom} AND am_thing_bottom <= {top})",
            bottom = at(reached::BOTTOMSLOPE),
            top = at(reached::TOPSLOPE),
        ),
    );
    value(
        "am_thing_blocks",
        format!(
            "toUInt8(am_shootable = 1 AND am_thing_top >= {slope} AND am_thing_bottom <= {slope})"
        ),
    );
    // `(thingtopslope + thingbottomslope) / 2`, each held inside the
    // window the lines left.
    value(
        "am_slope",
        format!(
            "toInt32(intDiv(toInt64(least(am_thing_top, {top})) \
             + toInt64(greatest(am_thing_bottom, {bottom})), 2))",
            top = at(reached::TOPSLOPE),
            bottom = at(reached::BOTTOMSLOPE),
        ),
    );

    // Where a shot that stops leaves its puff or its blood: a little short
    // of what stopped it, four units of the range for a line and ten for a
    // thing.
    value(
        "am_frac",
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
                "am_frac"
            )
        )
    };
    value("am_x", along("sh_tx", "sh_x2"));
    value("am_y", along("sh_ty", "sh_y2"));
    value(
        "am_z",
        format!(
            "toInt32(toInt64(am_shootz) + toInt64({}))",
            fixed::fixed_mul(&slope, &fixed::fixed_mul("am_frac", &ask(ask::RANGE)))
        ),
    );
    // The sky is not shot at.
    value(
        "am_sky",
        format!(
            "toUInt8(sec_ceilingpic[1 + line_front[1 + am_id]] = skyflatnum \
             AND (am_z > {ceiling}[1 + line_front[1 + am_id]] \
             OR (am_backless = 0 \
             AND sec_ceilingpic[1 + line_back[1 + am_id]] = skyflatnum)))",
            ceiling = world.ceilingheight,
        ),
    );
    // `P_ShootSpecialLine` runs for every line a shot crosses, the one it
    // ends on included.
    value(
        "am_spechit",
        format!(
            "if(am_in.{is_line} = 1 AND {special}[1 + am_id] != 0, \
             arrayPushBack({held}, am_id), {held})",
            is_line = intercept::IS_LINE,
            special = world.line_special,
            held = at(reached::SPECHIT),
        ),
    );

    // The answer is built one member at a time rather than as a tuple per
    // way out of an intercept. Six whole tuples of eleven members write
    // the carried ones out six times; eleven picks write each once.
    value(
        "am_stops",
        format!(
            "toUInt8(if({aims} = 1, \
             am_in.{is_line} = 1 AND am_line_stops = 1 OR am_in.{is_line} = 0 AND am_aimed = 1, \
             am_in.{is_line} = 1 AND am_line_blocks = 1 \
             OR am_in.{is_line} = 0 AND am_thing_blocks = 1))",
            aims = ask(ask::AIMS),
            is_line = intercept::IS_LINE,
        ),
    );
    // What an aim takes: the thing it stopped on.
    value(
        "am_takes",
        format!(
            "toUInt8({} = 1 AND am_in.{} = 0 AND am_aimed = 1)",
            ask(ask::AIMS),
            intercept::IS_LINE,
        ),
    );
    // What a shot ends on, and where. A line showing the sky stops it and
    // leaves nothing.
    value(
        "am_ends",
        format!(
            "toUInt8({} = 0 AND am_stops = 1 AND NOT (am_in.{} = 1 AND am_sky = 1))",
            ask(ask::AIMS),
            intercept::IS_LINE,
        ),
    );
    let keep = |field: usize| at(field);
    let member = |cast: &str, when: &str, value: &str, held: String| {
        format!("{cast}(if({when}, {value}, {held}))")
    };
    let members = [
        member("toInt32", "am_takes = 1", "am_slope", keep(reached::SLOPE)),
        member("toInt32", "am_takes = 1", "am_id", keep(reached::TARGET)),
        member(
            "toUInt8",
            "am_ends = 1",
            &format!("if(am_in.{} = 1, 1, 2)", intercept::IS_LINE),
            keep(reached::KIND),
        ),
        member("toInt32", "am_ends = 1", "am_id", keep(reached::ID)),
        member("toInt32", "am_ends = 1", "am_x", keep(reached::X)),
        member("toInt32", "am_ends = 1", "am_y", keep(reached::Y)),
        member("toInt32", "am_ends = 1", "am_z", keep(reached::Z)),
        "am_spechit".to_owned(),
        "toUInt8(am_stops)".to_owned(),
        // Only an aim narrows the window, and only on a line it crossed.
        member(
            "toInt32",
            &format!(
                "{} = 1 AND am_in.{} = 1 AND am_line_stops = 0",
                ask(ask::AIMS),
                intercept::IS_LINE
            ),
            "am_top",
            keep(reached::TOPSLOPE),
        ),
        member(
            "toInt32",
            &format!(
                "{} = 1 AND am_in.{} = 1 AND am_line_stops = 0",
                ask(ask::AIMS),
                intercept::IS_LINE
            ),
            "am_bottom",
            keep(reached::BOTTOMSLOPE),
        ),
    ];
    let body = format!(
        "if({stopped} = 1, am_at, ({}))",
        members.join(", "),
        stopped = at(reached::STOPPED),
    );
    let ran = format!(
        "arrayFold((am_at, am_in) -> {}, am_hits, \
         (toInt32(0), toInt32(0), toUInt8(0), toInt32(0), toInt32(0), toInt32(0), toInt32(0), \
         CAST([], 'Array(Int32)'), toUInt8(0), toInt32({TOPSLOPE}), toInt32({})))",
        bind::chain_in("ama", &values, &body),
        -TOPSLOPE,
    );
    let answer = bind::chain_in(
        "amv",
        &[("am_ran".to_owned(), ran)],
        &format!(
            "({})",
            (1..=reached::WIDTH)
                .map(|field| format!("am_ran.{field}"))
                .collect::<Vec<_>>()
                .join(", ")
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
    trace.push(("am_shootz".to_owned(), shoot_z(ask)));
    bind::chain_in("sho", &trace, &answer)
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

    /// One walk serves an aim and a shot, so the blockmap walk, the
    /// intercept fractions and their order are in the statement once
    /// however many of each a caller asks for.
    #[test]
    fn one_walk_serves_both_traversers() {
        let sql = traverse("asks", &world());
        assert_eq!(sql.matches("arrayFold((am_at, am_in)").count(), 1);
        assert_eq!(sql.matches("arrayMap((am_ask, am_hits)").count(), 1);
        assert_eq!(sql.matches("arrayFold((w, s)").count(), 1);
        assert_eq!(sql.matches("arrayFold((wk, c)").count(), 1);
    }

    /// The ask's own flag is what picks the traverser, so the walk does
    /// not have to know which of the two it is doing.
    #[test]
    fn the_ask_picks_the_traverser() {
        let sql = traverse("asks", &world());
        assert!(sql.contains(&format!("am_ask.{} = 1", ask::AIMS)), "{sql}");
        assert_eq!(
            asking("s", "x", "y", "z", "h", "a", "r")
                .matches("toUInt8(1)")
                .count(),
            1
        );
        assert_eq!(
            shooting("s", "x", "y", "z", "h", "a", "r", "sl")
                .matches("toUInt8(0)")
                .count(),
            1
        );
    }

    /// A fold body that reads neither of its parameters is evaluated
    /// outside the fold whatever the fold does.
    #[test]
    fn the_fold_body_reads_the_intercept_it_is_given() {
        let sql = traverse("asks", &world());
        let (_, body) = sql.split_once("arrayFold((am_at, am_in) -> ").unwrap();
        let (body, _) = body.split_once(", am_hits,").unwrap();
        assert!(body.contains("am_in."), "{body}");
    }

    /// `P_BulletSlope` asks three angles and one walk.
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
            traverse("asks", &world()),
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
