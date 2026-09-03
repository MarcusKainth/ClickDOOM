//! Line of sight between two things, from `p_sight.c`.
//!
//! `P_CheckSight` rejects the pair out of the `REJECT` matrix, then crosses
//! the BSP from the root and asks every subsector the trace passes through
//! whether a wall closes the view. The matrix answers for every pair asked
//! and the crossing runs only over the ones it lets through.
//!
//! Two things are written differently from the C and answer the same.
//!
//! The descent is a test over every subsector's ancestor path rather than a
//! walk. `P_CrossBSPNode` descends the child the trace starts on and the
//! other one only when the trace's two ends part over that partition line,
//! so a subsector is reached exactly when every ancestor on its path
//! satisfies one of those.
//!
//! `P_CrossSubsector`'s early return is not reproduced. It stops on a
//! one-sided line, on a closed opening, or when the top slope has fallen to
//! the bottom slope; the first two make the answer false wherever they are
//! met, and the two slopes only ever narrow, so testing them once over
//! every seg the trace reaches gives what the walk gives. `validcount`
//! keeps the walk off a line it has already crossed, and a line contributes
//! the same opening whichever of its segs is met, so that is not reproduced
//! either.

use crate::sql::{bind, fixed};

/// `doomdata.h`
const ML_TWOSIDED: i64 = 4;

/// The level tables a sight check reads, as constant arrays. Each is
/// indexed by its own id plus one unless the name says otherwise.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let seg = |column: &str| super::table_column(db, "lv_segs", column);
    let path = |column: &str| {
        format!(
            "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((subsector, {column}))))\
             \n     FROM {db}.lv_ssec_path)"
        )
    };
    // A run of one value per step of one subsector's path, 1 at the step
    // the marker names. Laid end to end these pick the running count of
    // shut steps out from either end of each subsector's own path.
    let marker = |at: &str| {
        format!(
            "arrayFlatten(arrayMap(d -> arrayMap(i -> toUInt8({at}), range(1, 1 + d)), {}))",
            path("depth")
        )
    };
    vec![
        (
            "reject_bits".to_owned(),
            format!("assumeNotNull((SELECT bits FROM {db}.lv_reject LIMIT 1))"),
        ),
        (
            "numsectors".to_owned(),
            format!("toInt64(assumeNotNull((SELECT count() FROM {db}.lv_sectors_static)))"),
        ),
        // Every subsector's ancestor path, laid end to end, so one pass
        // over the flat list answers for all of them.
        (
            "path_node".to_owned(),
            format!("arrayFlatten({})", path("nodes")),
        ),
        (
            "path_side".to_owned(),
            format!("arrayFlatten({})", path("sides")),
        ),
        (
            "path_before".to_owned(),
            format!("arrayPushBack({}, toUInt8(0))", marker("i = 1")),
        ),
        (
            "path_after".to_owned(),
            format!("arrayPushFront({}, toUInt8(0))", marker("i = d")),
        ),
        ("seg_line".to_owned(), seg("linedef")),
        ("seg_front".to_owned(), seg("frontsector")),
        ("seg_back".to_owned(), seg("backsector")),
        // Which subsector each seg belongs to: `firstline` and `numlines`
        // cut the seg list into subsectors in order.
        (
            "seg_subsector".to_owned(),
            format!(
                "arrayFlatten(arrayMap(t -> arrayMap(n -> t.1, range(t.3)), \
                 arraySort(t -> t.2, (SELECT groupArray((id, firstline, numlines)) \
                 FROM {db}.lv_subsectors))))"
            ),
        ),
        ("seg_twosided".to_owned(), {
            let flags = super::table_column(db, "lv_lines", "flags");
            format!(
                "arrayMap(l -> toUInt8(bitAnd(({flags})[1 + l], {ML_TWOSIDED}) != 0), {})",
                seg("linedef")
            )
        }),
    ]
}

/// Where each per-seg value sits in the tuple the seg walk is handed.
mod seg {
    pub const LINE: usize = 1;
    pub const OPENTOP: usize = 2;
    pub const OPENBOTTOM: usize = 3;
    pub const STEPPED: usize = 4;
    pub const DUCKED: usize = 5;
    pub const TWOSIDED: usize = 6;
}

/// Where each field of a pair sits in its tuple.
pub mod ask {
    /// The subsector each thing stands in, which is what the engine reads
    /// the sector out of.
    pub const SUBSECTOR1: usize = 1;
    pub const X1: usize = 2;
    pub const Y1: usize = 3;
    pub const Z1: usize = 4;
    pub const HEIGHT1: usize = 5;
    pub const SUBSECTOR2: usize = 6;
    pub const X2: usize = 7;
    pub const Y2: usize = 8;
    pub const Z2: usize = 9;
    pub const HEIGHT2: usize = 10;
}

/// One pair to check, as the tuple [`check_sight`] reads.
#[allow(clippy::too_many_arguments)]
pub fn asking(
    subsector1: &str,
    x1: &str,
    y1: &str,
    z1: &str,
    height1: &str,
    subsector2: &str,
    x2: &str,
    y2: &str,
    z2: &str,
    height2: &str,
) -> String {
    format!(
        "(toInt64({subsector1}), toInt64({x1}), toInt64({y1}), toInt64({z1}), toInt64({height1}), \
         toInt64({subsector2}), toInt64({x2}), toInt64({y2}), toInt64({z2}), toInt64({height2}))"
    )
}

/// The sector heights a sight check reads, which move as planes do.
pub struct Heights<'a> {
    pub floorheight: &'a str,
    pub ceilingheight: &'a str,
}

/// What every seg divides, worked out once for the whole tic.
///
/// A lambda copies an array it reads from outside itself once per element
/// of the array it walks, and copies nothing it is handed as a parameter.
/// The heights are the only thing a sight check reads that moves, so they
/// are turned into one value per seg here and the check walks those.
pub fn seg_openings(heights: &Heights<'_>) -> Vec<(String, String)> {
    // A one-sided seg names no back sector. Its line stops the view before
    // the heights are read, so the index is held on the front sector.
    let back = "1 + greatest(back, front)";
    let front = "1 + front";
    vec![
        (
            "seg_opentop".to_owned(),
            format!(
                "arrayMap((front, back) -> toInt64(least({c}[{front}], {c}[{back}])), \
                 seg_front, seg_back)",
                c = heights.ceilingheight
            ),
        ),
        (
            "seg_openbottom".to_owned(),
            format!(
                "arrayMap((front, back) -> toInt64(greatest({f}[{front}], {f}[{back}])), \
                 seg_front, seg_back)",
                f = heights.floorheight
            ),
        ),
        (
            "seg_stepped".to_owned(),
            format!(
                "arrayMap((front, back) -> toUInt8({f}[{front}] != {f}[{back}]), \
                 seg_front, seg_back)",
                f = heights.floorheight
            ),
        ),
        (
            "seg_ducked".to_owned(),
            format!(
                "arrayMap((front, back) -> toUInt8({c}[{front}] != {c}[{back}]), \
                 seg_front, seg_back)",
                c = heights.ceilingheight
            ),
        ),
        // What a seg divides, in one value per seg, so a check walks one
        // array and copies none of them.
        (
            "seg_facts".to_owned(),
            "arrayZip(seg_line, seg_opentop, seg_openbottom, seg_stepped, seg_ducked, \
             seg_twosided)"
                .to_owned(),
        ),
    ]
}

/// `P_CheckSight` over every pair in `pairs`: 1 where the first thing can
/// see the second.
///
/// The expression appears once however many pairs it is given, and reads
/// the per-seg openings [`seg_openings`] names.
pub fn check_sight(pairs: &str) -> String {
    let a = |field: usize| format!("sight.{field}");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    // The trace, and the slopes the view opens between.
    value("si_dx", format!("toInt64({} - {})", a(ask::X2), a(ask::X1)));
    value("si_dy", format!("toInt64({} - {})", a(ask::Y2), a(ask::Y1)));
    value(
        "si_zstart",
        format!(
            "toInt32({z} + {h} - bitShiftRight(toInt64({h}), 2))",
            z = a(ask::Z1),
            h = a(ask::HEIGHT1)
        ),
    );
    value(
        "si_top0",
        format!(
            "toInt32({} + {} - toInt64(si_zstart))",
            a(ask::Z2),
            a(ask::HEIGHT2)
        ),
    );
    value(
        "si_bottom0",
        format!("toInt32({} - toInt64(si_zstart))", a(ask::Z2)),
    );

    // One step of every subsector's ancestor path: whether the descent
    // turns away from the subsector there. `P_CrossBSPNode` reads the
    // start's side as a front when the point sits on the partition line,
    // and crosses to the other child only when the two ends part.
    let node = |column: &str| format!("{column}[1 + pnode]");
    let side = |x: &str, y: &str| {
        divline_side(
            x,
            y,
            &node("node_x"),
            &node("node_y"),
            &node("node_dx"),
            &node("node_dy"),
        )
    };
    let step = bind::chain_in(
        "ps",
        &[
            (
                "ps_side1".to_owned(),
                format!(
                    "toInt64(if({s} = 2, 0, {s}))",
                    s = side(&a(ask::X1), &a(ask::Y1))
                ),
            ),
            (
                "ps_side2".to_owned(),
                format!("toInt64({})", side(&a(ask::X2), &a(ask::Y2))),
            ),
        ],
        "toUInt8(NOT (pside = ps_side1 OR ps_side1 != ps_side2))",
    );
    value(
        "si_path_shut",
        format!("arrayMap((pnode, pside) -> {step}, path_node, path_side)"),
    );
    // A subsector the trace reaches has no shut step on its path, which is
    // the running count of shut steps standing still across it.
    value(
        "si_shut_upto",
        "arrayPushFront(arrayCumSum(si_path_shut), toUInt64(0))".to_owned(),
    );
    value(
        "si_reached",
        "arrayMap((b, a) -> toUInt8(a = b), \
         arrayFilter((c, m) -> m = 1, si_shut_upto, path_before), \
         arrayFilter((c, m) -> m = 1, si_shut_upto, path_after))"
            .to_owned(),
    );
    // The segs of the subsectors the trace reaches, which is where the
    // whole cost of a check goes, so the ones it does not reach are
    // dropped before the crossing runs.
    value(
        "si_segs",
        "arrayFilter((f, ss) -> si_reached[1 + ss] = 1, seg_facts, seg_subsector)".to_owned(),
    );

    // `P_CrossSubsector` for every seg of every subsector the trace
    // reaches, as what the line does to the view.
    value(
        "si_crossed",
        crossing(&a(ask::X1), &a(ask::Y1), &a(ask::X2), &a(ask::Y2)),
    );
    value(
        "si_top",
        "toInt64(arrayMin(arrayPushBack(arrayMap(c -> c.2, si_crossed), toInt64(si_top0))))"
            .to_owned(),
    );
    value(
        "si_bottom",
        "toInt64(arrayMax(arrayPushBack(arrayMap(c -> c.3, si_crossed), toInt64(si_bottom0))))"
            .to_owned(),
    );
    value(
        "si_shut",
        "toUInt8(arrayExists(c -> c.1 = 1, si_crossed))".to_owned(),
    );

    let body = "toUInt8(si_shut = 0 AND si_top > si_bottom)";
    // The matrix rejects most pairs, and the crossing above is the whole
    // cost of a check, so it runs over the ones it lets through. The
    // running count of those puts each answer back where its pair asked.
    let gated: Vec<(String, String)> = vec![
        ("sg_ask".to_owned(), pairs.to_owned()),
        (
            "sg_rejected".to_owned(),
            format!("arrayMap(sight -> {}, sg_ask)", rejected(&a)),
        ),
        (
            "sg_open".to_owned(),
            "arrayFilter((p, r) -> r = 0, sg_ask, sg_rejected)".to_owned(),
        ),
        (
            "sg_seen".to_owned(),
            format!(
                "arrayMap(sight -> {}, sg_open)",
                bind::chain_in("si", &values, body)
            ),
        ),
        (
            "sg_at".to_owned(),
            "arrayCumSum(arrayMap(r -> toUInt8(r = 0), sg_rejected))".to_owned(),
        ),
    ];
    bind::chain_in(
        "sg",
        &gated,
        "arrayMap((r, i) -> toUInt8(r = 0 AND sg_seen[greatest(i, 1)] = 1), \
         sg_rejected, sg_at)",
    )
}

/// The trivial rejection, out of the matrix the level carries: 1 where the
/// two sectors cannot possibly be connected.
fn rejected(a: &dyn Fn(usize) -> String) -> String {
    let pnum = format!(
        "toInt64(toInt64(ssec_sector[1 + {}]) * numsectors + toInt64(ssec_sector[1 + {}]))",
        a(ask::SUBSECTOR1),
        a(ask::SUBSECTOR2)
    );
    bind::chain_in(
        "rj",
        &[("rj_pnum".to_owned(), pnum)],
        "toUInt8(bitAnd(reinterpretAsUInt8(substring(reject_bits, \
         1 + intDiv(rj_pnum, 8), 1)), bitShiftLeft(1, modulo(rj_pnum, 8))) != 0)",
    )
}

/// `P_CrossSubsector`'s body over every seg, as what each does to the
/// view: whether it shuts it, and the top and bottom slopes it leaves.
///
/// A seg the trace does not cross, or whose subsector it does not reach,
/// leaves both slopes where they were, as the `continue` does. Every array
/// it walks is a parameter, so nothing is copied per seg.
fn crossing(x1: &str, y1: &str, x2: &str, y2: &str) -> String {
    let line = |column: &str| format!("{column}[1 + cr_line]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("cr_line", format!("toInt64(cross.{})", seg::LINE));
    // The trace crosses the line and the line crosses the trace.
    let trace_side = |x: &str, y: &str| divline_side(x, y, x1, y1, "si_dx", "si_dy");
    value(
        "cr_ends_part",
        format!(
            "toUInt8({} != {})",
            trace_side(&line("line_v1x"), &line("line_v1y")),
            trace_side(&line("line_v2x"), &line("line_v2y"))
        ),
    );
    let line_side = |x: &str, y: &str| {
        divline_side(
            x,
            y,
            &line("line_v1x"),
            &line("line_v1y"),
            &line("line_dx"),
            &line("line_dy"),
        )
    };
    value(
        "cr_crosses",
        format!(
            "toUInt8(cr_ends_part = 1 AND {} != {})",
            line_side(x1, y1),
            line_side(x2, y2)
        ),
    );
    value(
        "cr_solid",
        format!("toUInt8(cr_crosses = 1 AND cross.{} = 0)", seg::TWOSIDED),
    );
    // A seg whose two sides stand at the same heights is no wall at all.
    value(
        "cr_wall",
        format!(
            "toUInt8(cr_crosses = 1 AND cross.{} = 1 AND (cross.{} = 1 OR cross.{} = 1))",
            seg::TWOSIDED,
            seg::STEPPED,
            seg::DUCKED
        ),
    );
    value(
        "cr_shut",
        format!(
            "toUInt8(cr_solid = 1 OR (cr_wall = 1 AND cross.{} >= cross.{}))",
            seg::OPENBOTTOM,
            seg::OPENTOP
        ),
    );
    value(
        "cr_narrows",
        format!(
            "toUInt8(cr_wall = 1 AND cross.{} < cross.{})",
            seg::OPENBOTTOM,
            seg::OPENTOP
        ),
    );
    value(
        "cr_frac",
        format!(
            "toInt32({})",
            intercept_vector(
                x1,
                y1,
                "si_dx",
                "si_dy",
                &line("line_v1x"),
                &line("line_v1y"),
                &line("line_dx"),
                &line("line_dy")
            )
        ),
    );
    let slope = |height: usize| {
        fixed::fixed_div(
            &format!("toInt32(cross.{height} - toInt64(si_zstart))"),
            "cr_frac",
        )
    };
    value(
        "cr_top",
        format!(
            "toInt64(if(cr_narrows = 1 AND cross.{} = 1, toInt64({}), toInt64(si_top0)))",
            seg::DUCKED,
            slope(seg::OPENTOP)
        ),
    );
    value(
        "cr_bottom",
        format!(
            "toInt64(if(cr_narrows = 1 AND cross.{} = 1, toInt64({}), toInt64(si_bottom0)))",
            seg::STEPPED,
            slope(seg::OPENBOTTOM)
        ),
    );

    let body = "(cr_shut, cr_top, cr_bottom)";
    format!(
        "arrayMap(cross -> {}, si_segs)",
        bind::chain_in("cr", &values, body)
    )
}

/// `P_DivlineSide`: which side of the line through `(lx, ly)` running
/// `(ldx, ldy)` the point `(x, y)` lies on, 2 for on the line.
///
/// The second axis-aligned case compares the point's `x` against the
/// line's `y`, which is what `p_sight.c` does.
pub fn divline_side(x: &str, y: &str, lx: &str, ly: &str, ldx: &str, ldy: &str) -> String {
    let product = |a: String, b: String| format!("toInt32(toInt64({a}) * toInt64({b}))");
    let left = product(
        format!("bitShiftRight(toInt64({ldy}), 16)"),
        format!("bitShiftRight(toInt64({x}) - toInt64({lx}), 16)"),
    );
    let right = product(
        format!("bitShiftRight(toInt64({y}) - toInt64({ly}), 16)"),
        format!("bitShiftRight(toInt64({ldx}), 16)"),
    );
    format!(
        "multiIf(\
         {ldx} = 0, multiIf({x} = {lx}, 2, {x} <= {lx}, if({ldy} > 0, 1, 0), if({ldy} < 0, 1, 0)), \
         {ldy} = 0, multiIf({x} = {ly}, 2, {y} <= {ly}, if({ldx} < 0, 1, 0), if({ldx} > 0, 1, 0)), \
         {right} < {left}, 0, {left} = {right}, 2, 1)"
    )
}

/// `P_InterceptVector2`: how far along the line the trace crosses it.
///
/// The engine passes the trace as `v2` and the line as `v1`, and the
/// fraction is along `v1`.
#[allow(clippy::too_many_arguments)]
fn intercept_vector(
    trace_x: &str,
    trace_y: &str,
    trace_dx: &str,
    trace_dy: &str,
    line_x: &str,
    line_y: &str,
    line_dx: &str,
    line_dy: &str,
) -> String {
    let den = format!(
        "toInt32(toInt64({}) - toInt64({}))",
        fixed::fixed_mul(&format!("bitShiftRight(toInt64({line_dy}), 8)"), trace_dx),
        fixed::fixed_mul(&format!("bitShiftRight(toInt64({line_dx}), 8)"), trace_dy)
    );
    let num = format!(
        "toInt32(toInt64({}) + toInt64({}))",
        fixed::fixed_mul(
            &format!("bitShiftRight(toInt64({line_x}) - toInt64({trace_x}), 8)"),
            line_dy
        ),
        fixed::fixed_mul(
            &format!("bitShiftRight(toInt64({trace_y}) - toInt64({line_y}), 8)"),
            line_dx
        )
    );
    format!(
        "if(({den}) = 0, toInt32(0), {})",
        fixed::fixed_div(&format!("({num})"), &format!("({den})"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_appears_once_however_many_pairs_it_is_given() {
        let sql = check_sight("pending");
        assert_eq!(sql.matches("arrayMap(cross ->").count(), 1);
        assert_eq!(sql.matches("reject_bits").count(), 1);
    }

    /// The matrix answers for every pair and the crossing only for the
    /// ones it lets through, so the two walk different lists.
    #[test]
    fn the_crossing_runs_over_the_pairs_reject_lets_through() {
        let sql = check_sight("pending");
        assert_eq!(
            sql.matches("arrayMap(sight ->").count(),
            2,
            "the matrix over every pair and the crossing over the rest"
        );
        assert_eq!(sql.matches("arrayFilter((p, r) -> r = 0, ").count(), 1);
    }

    /// The seg walk takes what it reads through the array it walks, so a
    /// lambda copies nothing per seg, and it walks the segs the trace
    /// reaches rather than every seg of the level.
    #[test]
    fn the_seg_walk_takes_its_arrays_as_parameters() {
        let sql = check_sight("pending");
        assert_eq!(sql.matches("seg_facts").count(), 1, "{sql}");
        assert_eq!(sql.matches("arrayFilter((f, ss) ->").count(), 1, "{sql}");
        let openings = seg_openings(&Heights {
            floorheight: "w_floor",
            ceilingheight: "w_ceiling",
        });
        let (_, zip) = openings
            .iter()
            .find(|(name, _)| name == "seg_facts")
            .expect("the per-tic facts");
        for array in ["seg_opentop", "seg_openbottom", "seg_stepped", "seg_ducked"] {
            assert_eq!(zip.matches(array).count(), 1, "{array} is read once");
        }
    }

    /// `P_DivlineSide` compares the point's `x` against the line's `y` in
    /// its second case. It is a typo in the engine and the answers depend
    /// on it.
    #[test]
    fn the_side_test_keeps_the_engine_s_second_case() {
        let sql = divline_side("px", "py", "lx", "ly", "ldx", "ldy");
        assert!(sql.contains("ldy = 0, multiIf(px = ly, 2,"), "{sql}");
    }

    #[test]
    fn the_expression_balances_its_parentheses() {
        let sql = check_sight("pending");
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
    }
}
