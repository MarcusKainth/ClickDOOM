//! Where things are, from `p_maputl.c`.
//!
//! The blockmap groups the level into 128-unit cells so a move only has to
//! look at what is near it. Everything here answers a question about a
//! point, a box or a line, and none of it changes anything.

use super::table_column;

/// `p_local.h`: the blockmap's cell size, as the shift that turns a
/// `fixed_t` into a cell number.
pub const MAPBLOCKSHIFT: u32 = 16 + 7;
/// `p_local.h`: the furthest a thing's edge reaches from the cell its
/// origin sits in.
pub const MAXRADIUS: i64 = 32 << 16;

/// `r_defs.h`: which way a line runs, which is what `P_BoxOnLineSide`
/// switches on.
const ST_HORIZONTAL: i32 = 0;
const ST_VERTICAL: i32 = 1;
const ST_POSITIVE: i32 = 2;

/// `doomdata.h`: the four sides of a bounding box, in the engine's order.
pub const BOX_TOP: usize = 1;
pub const BOX_BOTTOM: usize = 2;
pub const BOX_LEFT: usize = 3;
pub const BOX_RIGHT: usize = 4;

/// The map tables a move reads, as constant arrays indexed by id plus one.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let line = |column: &str| table_column(db, "lv_lines", column);
    let node = |column: &str| table_column(db, "lv_nodes", column);
    let mut constants = vec![
        ("line_v1x".to_owned(), vertex(db, "v1", "x")),
        ("line_v1y".to_owned(), vertex(db, "v1", "y")),
        ("line_v2x".to_owned(), vertex(db, "v2", "x")),
        ("line_v2y".to_owned(), vertex(db, "v2", "y")),
        ("line_dx".to_owned(), line("dx")),
        ("line_dy".to_owned(), line("dy")),
        ("line_flags".to_owned(), line("flags")),
        ("line_slopetype".to_owned(), line("slopetype")),
        ("line_side1".to_owned(), line("side1")),
        ("line_front".to_owned(), line("sector0")),
        ("line_back".to_owned(), line("sector1")),
        ("line_static_special".to_owned(), line("special")),
        ("node_x".to_owned(), node("x")),
        ("node_y".to_owned(), node("y")),
        ("node_dx".to_owned(), node("dx")),
        ("node_dy".to_owned(), node("dy")),
        ("node_child0".to_owned(), node("children[1]")),
        ("node_child1".to_owned(), node("children[2]")),
        (
            "numnodes".to_owned(),
            format!("assumeNotNull((SELECT count() FROM {db}.lv_nodes))"),
        ),
        (
            "bsp_depth".to_owned(),
            format!("assumeNotNull((SELECT max(depth) FROM {db}.lv_ssec_path))"),
        ),
        (
            "ssec_sector".to_owned(),
            table_column(db, "lv_subsectors", "sector"),
        ),
        ("finesine".to_owned(), table_column(db, "finesine", "value")),
    ];
    for (at, side) in [BOX_TOP, BOX_BOTTOM, BOX_LEFT, BOX_RIGHT]
        .into_iter()
        .enumerate()
    {
        let name = ["line_top", "line_bottom", "line_left", "line_right"][at];
        constants.push((name.to_owned(), line(&format!("bbox[{side}]"))));
    }
    for (name, column) in [
        ("bmap_orgx", "origin_x"),
        ("bmap_orgy", "origin_y"),
        ("bmap_cols", "columns"),
        ("bmap_rows", "rows"),
    ] {
        constants.push((
            name.to_owned(),
            format!(
                "toInt64(assumeNotNull((SELECT {column} FROM {db}.lv_blockmap_header LIMIT 1)))"
            ),
        ));
    }
    constants.push((
        "bmap_lines".to_owned(),
        format!(
            "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((cell, lines))))\
             \n     FROM {db}.lv_blockmap)"
        ),
    ));
    constants
}

/// One end of every line, which the side tests compare a point against.
fn vertex(db: &str, end: &str, axis: &str) -> String {
    format!(
        "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((l.id, v.{axis}))))\
         \n     FROM {db}.lv_lines AS l INNER JOIN {db}.lv_vertexes AS v ON v.id = l.{end})"
    )
}

/// `finecosine`, which the engine keeps as `finesine` a quarter turn on.
pub fn finecosine(angle: &str) -> String {
    format!("finesine[1 + bitAnd(({angle}) + 2048, 8191)]")
}

/// `finesine` at a fine angle.
pub fn finesine(angle: &str) -> String {
    format!("finesine[1 + bitAnd({angle}, 8191)]")
}

/// The blockmap cells a box covers, in the order the engine walks them:
/// one column at a time, bottom to top inside each.
///
/// `margin` widens the box, which the things pass does by `MAXRADIUS`
/// because a thing is filed under the cell its origin sits in and reaches
/// out of it. A cell off the map is left out, which is what the iterators
/// return early for.
pub fn cells(bbox: &str, margin: i64) -> String {
    let at = |side: usize, sign: i64| {
        format!(
            "bitShiftRight(toInt64({bbox}[{side}]) - {} + {}, {MAPBLOCKSHIFT})",
            if side == BOX_LEFT || side == BOX_RIGHT {
                "bmap_orgx"
            } else {
                "bmap_orgy"
            },
            sign * margin
        )
    };
    let (xl, xh) = (at(BOX_LEFT, -1), at(BOX_RIGHT, 1));
    let (yl, yh) = (at(BOX_BOTTOM, -1), at(BOX_TOP, 1));
    format!(
        "arrayFlatten(arrayMap(bx -> arrayMap(by -> by * bmap_cols + bx, \
         range(greatest({yl}, 0), least({yh}, bmap_rows - 1) + 1)), \
         range(greatest({xl}, 0), least({xh}, bmap_cols - 1) + 1)))"
    )
}

/// The lines in `cells`, in first-occurrence order.
///
/// `P_BlockLinesIterator` skips a line it has already seen this call, which
/// is what the engine's `validcount` does and what dropping later
/// occurrences does here.
pub fn lines_in(cells: &str) -> String {
    format!(
        "arrayDistinct(arrayFlatten(arrayMap(c -> arrayMap(l -> toInt32(l), bmap_lines[1 + c]), {cells})))"
    )
}

/// `P_BoxOnLineSide`: which side of `line` the whole box lies on, or -1
/// when the line crosses it.
pub fn box_on_line_side(bbox: &str, line: &str) -> String {
    let at = |side: usize| format!("{bbox}[{side}]");
    let field = |name: &str| format!("{name}[1 + {line}]");
    let (dx, dy) = (field("line_dx"), field("line_dy"));
    let (v1x, v1y) = (field("line_v1x"), field("line_v1y"));
    let side = |x: String, y: String| {
        super::super::fixed::point_on_line_side(&x, &y, &v1x, &v1y, &dx, &dy)
    };
    // The two corners each case compares, as `p1` then `p2`.
    let horizontal = format!(
        "(toUInt8({} > {v1y}), toUInt8({} > {v1y}))",
        at(BOX_TOP),
        at(BOX_BOTTOM)
    );
    let vertical = format!(
        "(toUInt8({} < {v1x}), toUInt8({} < {v1x}))",
        at(BOX_RIGHT),
        at(BOX_LEFT)
    );
    // The two sloping cases take the top corner and then the bottom one
    // and differ only in which side of the box each takes its `x` from, so
    // they are one pair. That halves the calls to `P_PointOnLineSide`,
    // which is the largest expression this holds.
    let corner_x = |positive: String, negative: String| {
        format!(
            "if({} = {ST_POSITIVE}, {positive}, {negative})",
            field("line_slopetype")
        )
    };
    let sloping = format!(
        "({}, {})",
        side(corner_x(at(BOX_LEFT), at(BOX_RIGHT)), at(BOX_TOP)),
        side(corner_x(at(BOX_RIGHT), at(BOX_LEFT)), at(BOX_BOTTOM))
    );
    // A horizontal line running left and a vertical line running down have
    // their sides the other way round.
    let flip = format!(
        "toUInt8(({} = {ST_HORIZONTAL} AND {dx} < 0) OR ({} = {ST_VERTICAL} AND {dy} < 0))",
        field("line_slopetype"),
        field("line_slopetype")
    );
    // The case picks the pair, and the pair is named once. Reading a
    // corner out of each case on its own writes every case a second time.
    let corners = format!(
        "multiIf({slope} = {ST_HORIZONTAL}, {horizontal}, \
         {slope} = {ST_VERTICAL}, {vertical}, {sloping}) AS box_corners",
        slope = field("line_slopetype")
    );
    // The two corners land on the same side or they do not, and the flip
    // does not change whether they agree, so it is applied once at the end
    // rather than to each corner.
    format!(
        "toInt32(multiIf((toInt16(({corners}).1) + toInt16(box_corners.2)) AS box_sides = 0, \
         toInt16(bitXor(0, {flip})), \
         box_sides = 2, toInt16(bitXor(1, {flip})), toInt16(-1)))"
    )
}

/// `P_LineOpening`'s three heights, as one tuple `(top, bottom, lowfloor)`.
///
/// A one-sided line has no opening, and the engine leaves the three at
/// whatever the last two-sided line put there. Nothing reads them in that
/// case, so this returns the closed window instead.
pub fn opening(line: &str, floorheight: &str, ceilingheight: &str) -> String {
    let field = |name: &str| format!("{name}[1 + {line}]");
    let front = format!("1 + {}", field("line_front"));
    let back = format!("1 + {}", field("line_back"));
    let (ffloor, bfloor) = (
        format!("{floorheight}[{front}]"),
        format!("{floorheight}[{back}]"),
    );
    let (fceil, bceil) = (
        format!("{ceilingheight}[{front}]"),
        format!("{ceilingheight}[{back}]"),
    );
    format!(
        "if({} = -1, (toInt32(0), toInt32(0), toInt32(0)), \
         (least({fceil}, {bceil}), greatest({ffloor}, {bfloor}), \
         if({ffloor} > {bfloor}, {bfloor}, {ffloor})))",
        field("line_side1")
    )
}

/// `p_maputl.c`: how many blocks a trace walks before it gives up.
const TRAVERSE_BLOCKS: i64 = 64;
/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `p_local.h`: the shift that turns a `fixed_t` into a block-relative
/// fraction.
const MAPBTOFRAC: u32 = MAPBLOCKSHIFT - 16;
const MAPBLOCKSIZE: i64 = 1 << MAPBLOCKSHIFT;

/// Where each field of a trace sits in its tuple.
pub mod trace {
    pub const X1: usize = 1;
    pub const Y1: usize = 2;
    pub const X2: usize = 3;
    pub const Y2: usize = 4;
}

/// Where each field of an intercept sits in its tuple.
pub mod intercept {
    /// The line, or the mobj slot when [`IS_LINE`] is 0.
    pub const ID: usize = 1;
    pub const FRAC: usize = 2;
    pub const IS_LINE: usize = 3;
}

/// The mobj arrays `PIT_AddThingIntercepts` reads.
pub struct Things<'a> {
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_radius: &'a str,
    pub m_linkseq: &'a str,
    /// One per mobj slot: 1 while it is still on the list.
    pub alive: &'a str,
}

/// One trace, as the tuple [`path_traverse`] reads.
pub fn tracing(x1: &str, y1: &str, x2: &str, y2: &str) -> String {
    format!("(toInt64({x1}), toInt64({y1}), toInt64({x2}), toInt64({y2}))")
}

/// Where a trace really starts.
///
/// `P_PathTraverse` moves a start that sits exactly on a block edge one
/// unit off it, so the walk does not have to decide which side it is on,
/// and the divline it hands a traverser starts there.
pub fn nudged(coord: &str, origin: &str) -> String {
    format!(
        "toInt64({coord} + if(bitAnd({coord} - {origin}, {}) = 0, {FRACUNIT}, 0))",
        MAPBLOCKSIZE - 1
    )
}

/// `P_PathTraverse` over an array of traces, with `PT_ADDLINES` always and
/// `PT_ADDTHINGS` when `things` is given.
///
/// Each answer is what the trace crosses, in the order
/// `P_TraverseIntercepts` hands it to a traverser: nearest first, and among
/// equal fractions the one the block walk found first. An intercept past
/// the end of the trace is left out, which is what the walk's own `maxfrac`
/// does.
///
/// The walk visits a block's lines and then its things, so an intercept's
/// place among equal fractions depends on which block it was found in.
/// That is why the two lists are built per block and concatenated rather
/// than gathered separately.
///
/// `PT_EARLYOUT` is not here. No caller in the engine asks for it.
pub fn path_traverse(traces: &str, things: Option<&Things<'_>>) -> String {
    let t = |field: usize| format!("tr.{field}");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("pt_x1", nudged(&t(trace::X1), "bmap_orgx"));
    value("pt_y1", nudged(&t(trace::Y1), "bmap_orgy"));
    value("pt_dx", format!("toInt64({} - pt_x1)", t(trace::X2)));
    value("pt_dy", format!("toInt64({} - pt_y1)", t(trace::Y2)));
    // The walk is in block coordinates from the blockmap's own origin.
    value("pt_rx1", "toInt64(pt_x1 - bmap_orgx)".to_owned());
    value("pt_ry1", "toInt64(pt_y1 - bmap_orgy)".to_owned());
    value("pt_rx2", format!("toInt64({} - bmap_orgx)", t(trace::X2)));
    value("pt_ry2", format!("toInt64({} - bmap_orgy)", t(trace::Y2)));
    value("pt_xt1", format!("bitShiftRight(pt_rx1, {MAPBLOCKSHIFT})"));
    value("pt_yt1", format!("bitShiftRight(pt_ry1, {MAPBLOCKSHIFT})"));
    value("pt_xt2", format!("bitShiftRight(pt_rx2, {MAPBLOCKSHIFT})"));
    value("pt_yt2", format!("bitShiftRight(pt_ry2, {MAPBLOCKSHIFT})"));
    let fixed_div = |a: &str, b: &str| {
        super::super::fixed::fixed_div(&format!("toInt32({a})"), &format!("toInt32({b})"))
    };
    value(
        "pt_mapxstep",
        "toInt64(multiIf(pt_xt2 > pt_xt1, 1, pt_xt2 < pt_xt1, -1, 0))".to_owned(),
    );
    value(
        "pt_mapystep",
        "toInt64(multiIf(pt_yt2 > pt_yt1, 1, pt_yt2 < pt_yt1, -1, 0))".to_owned(),
    );
    value(
        "pt_ystep",
        format!(
            "toInt64(if(pt_mapxstep = 0, {}, {}))",
            256 * FRACUNIT,
            fixed_div("pt_ry2 - pt_ry1", "abs(pt_rx2 - pt_rx1)")
        ),
    );
    value(
        "pt_xstep",
        format!(
            "toInt64(if(pt_mapystep = 0, {}, {}))",
            256 * FRACUNIT,
            fixed_div("pt_rx2 - pt_rx1", "abs(pt_ry2 - pt_ry1)")
        ),
    );
    let partial = |mapstep: &str, rel: &str| {
        format!(
            "toInt64(multiIf({mapstep} > 0, {FRACUNIT} - bitAnd(bitShiftRight({rel}, {MAPBTOFRAC}), {}), \
             {mapstep} < 0, bitAnd(bitShiftRight({rel}, {MAPBTOFRAC}), {}), {FRACUNIT}))",
            FRACUNIT - 1,
            FRACUNIT - 1
        )
    };
    let fixed_mul = |a: &str, b: &str| format!("bitShiftRight(({a}) * ({b}), 16)");
    value(
        "pt_yintercept",
        format!(
            "toInt64(bitShiftRight(pt_ry1, {MAPBTOFRAC}) + {})",
            fixed_mul(&partial("pt_mapxstep", "pt_rx1"), "pt_ystep")
        ),
    );
    value(
        "pt_xintercept",
        format!(
            "toInt64(bitShiftRight(pt_rx1, {MAPBTOFRAC}) + {})",
            fixed_mul(&partial("pt_mapystep", "pt_ry1"), "pt_xstep")
        ),
    );
    // The block walk. The engine adds a block's lines at the top of each
    // pass, before it decides which way to step, so the walk records the
    // block it is standing in whatever it does next. A block off the map
    // is recorded as -1, because the iterators return early for one and
    // the walk carries on.
    let cell = "if(w.1 >= 0 AND w.1 < bmap_cols AND w.2 >= 0 AND w.2 < bmap_rows, \
                w.2 * bmap_cols + w.1, toInt64(-1))";
    let step = "multiIf(\
         w.1 = pt_xt2 AND w.2 = pt_yt2, (w.1, w.2, w.3, w.4, toUInt8(1), seen), \
         bitShiftRight(w.4, 16) = w.2, \
         (w.1 + pt_mapxstep, w.2, w.3, w.4 + pt_ystep, toUInt8(0), seen), \
         bitShiftRight(w.3, 16) = w.1, \
         (w.1, w.2 + pt_mapystep, w.3 + pt_xstep, w.4, toUInt8(0), seen), \
         (w.1, w.2, w.3, w.4, toUInt8(0), seen))";
    value(
        "pt_cells",
        format!(
            "arrayFilter(c -> c >= 0, arrayFold((w, s) -> if(w.5 = 1, w, \
             arrayMap(seen -> {step}, [arrayPushBack(w.6, {cell})])[1]), \
             range({TRAVERSE_BLOCKS}), \
             (pt_xt1, pt_yt1, pt_xintercept, pt_yintercept, toUInt8(0), \
             CAST([], 'Array(Int64)'))).6)"
        ),
    );
    // What the two iterators reach, as `(id, is_line)` in the order they
    // reach it.
    if let Some(things) = things {
        value("pt_near", near(things));
    }
    value("pt_at", walked(things));
    // `PIT_AddLineIntercepts`: a line is crossed when its ends fall on
    // opposite sides of the trace. A short trace compares the trace's own
    // ends against the line instead, which is what the engine does to
    // keep the two routines' precision apart.
    // A walked entry is `(id, is_line)`, which the answer keeps with the
    // fraction between them.
    let id = "at.1";
    let is_line = "at.2";
    let line = |name: &str| format!("{name}[1 + {id}]");
    let side_of_trace = |x: &str, y: &str| {
        super::super::fixed::point_on_side(x, y, "pt_x1", "pt_y1", "pt_dx", "pt_dy", 8)
    };
    let side_of_line = |x: &str, y: &str| {
        super::super::fixed::point_on_line_side(
            x,
            y,
            &line("line_v1x"),
            &line("line_v1y"),
            &line("line_dx"),
            &line("line_dy"),
        )
    };
    let long = format!(
        "pt_dx > {far} OR pt_dy > {far} OR pt_dx < -{far} OR pt_dy < -{far}",
        far = 16 * FRACUNIT
    );
    let crossed = format!(
        "if({long}, \
         {} != {}, \
         {} != {})",
        side_of_trace(&line("line_v1x"), &line("line_v1y")),
        side_of_trace(&line("line_v2x"), &line("line_v2y")),
        side_of_line("toInt32(pt_x1)", "toInt32(pt_y1)"),
        side_of_line("toInt32(pt_x1 + pt_dx)", "toInt32(pt_y1 + pt_dy)"),
    );
    let line_frac = format!(
        "if({crossed}, {}, -1)",
        super::super::fixed::intercept_vector(
            "toInt32(pt_x1)",
            "toInt32(pt_y1)",
            "toInt32(pt_dx)",
            "toInt32(pt_dy)",
            &line("line_v1x"),
            &line("line_v1y"),
            &line("line_dx"),
            &line("line_dy"),
        )
    );
    let hit = match things {
        Some(things) => format!("if({is_line} = 1, {line_frac}, {})", thing_frac(id, things)),
        None => line_frac,
    };
    value(
        "pt_hits",
        format!(
            "arrayFilter(h -> h.{frac} >= 0 AND h.{frac} <= {FRACUNIT}, \
             arrayMap(at -> ({id}, toInt32({hit}), {is_line}), pt_at))",
            frac = intercept::FRAC,
        ),
    );
    // `P_TraverseIntercepts` takes the nearest each time, and a strict
    // comparison leaves an equal fraction where the walk put it.
    let body = format!(
        "arrayMap(p -> p.2, arraySort(p -> (p.2.{}, p.1), \
         arrayMap((h, i) -> (i, h), pt_hits, arrayEnumerate(pt_hits))))",
        intercept::FRAC
    );
    format!(
        "arrayMap(tr -> {}, {traces})",
        crate::sql::bind::chain(&values, &body)
    )
}

/// What the block walk reaches over `pt_cells`, as `(id, is_line)` in the
/// order it reaches it.
///
/// `P_BlockLinesIterator` skips a line it has already reached this call,
/// which is `validcount`, so a lines-only walk is the distinct lines in
/// first-occurrence order. A walk that adds things has to keep the blocks
/// apart, because a block's things go in behind its lines, so it carries
/// the lines it has already reached from block to block.
fn walked(things: Option<&Things<'_>>) -> String {
    let Some(things) = things else {
        return format!(
            "arrayMap(l -> (toInt32(l), toUInt8(1)), {})",
            lines_in("pt_cells")
        );
    };
    // `P_BlockThingsIterator` walks the block's list, which
    // `P_SetThingPosition` puts each thing at the head of, so the one
    // linked last comes first.
    let in_block = format!(
        "arrayMap(k -> (toInt32(k), toUInt8(0)), \
         arraySort(k -> -toInt64({}[k]), arrayFilter(k -> {} = c, pt_near)))",
        things.m_linkseq,
        cell_of("k", things),
    );
    format!(
        "arrayFold((wk, c) -> (\
         arrayConcat(wk.1, \
         arrayMap(l -> (toInt32(l), toUInt8(1)), \
         arrayFilter(l -> NOT has(wk.2, l), arrayDistinct(bmap_lines[1 + c]))), \
         {in_block}), \
         arrayConcat(wk.2, bmap_lines[1 + c])), \
         pt_cells, \
         (CAST([], 'Array(Tuple(Int32, UInt8))'), CAST([], 'Array(UInt16)'))).1"
    )
}

/// The blockmap cell a mobj is filed under.
fn cell_of(slot: &str, things: &Things<'_>) -> String {
    format!(
        "(bitShiftRight(toInt64({}[{slot}]) - bmap_orgy, {MAPBLOCKSHIFT}) * bmap_cols + \
         bitShiftRight(toInt64({}[{slot}]) - bmap_orgx, {MAPBLOCKSHIFT}))",
        things.m_y, things.m_x,
    )
}

/// Every mobj the walk could reach, filtered once for the trace rather
/// than once per block, so the list each block sorts is short.
fn near(things: &Things<'_>) -> String {
    format!(
        "arrayFilter(k -> {alive}[k] = 1 AND has(pt_cells, {}), arrayEnumerate({alive}))",
        cell_of("k", things),
        alive = things.alive,
    )
}

/// `PIT_AddThingIntercepts`: where the trace crosses the thing's box, or
/// -1 where it does not cross it or crosses it behind the source.
///
/// The box is one diagonal, and which diagonal depends on which way the
/// trace runs, so the near corner is the one the trace meets first.
fn thing_frac(slot: &str, things: &Things<'_>) -> String {
    let at = |array: &str| format!("toInt64({array}[{slot}])");
    let (x, y, radius) = (at(things.m_x), at(things.m_y), at(things.m_radius));
    let side = |x: &str, y: &str| {
        super::super::fixed::point_on_side(x, y, "pt_x1", "pt_y1", "pt_dx", "pt_dy", 8)
    };
    let values = vec![
        (
            "th_positive".to_owned(),
            "toUInt8(bitXor(toInt32(pt_dx), toInt32(pt_dy)) > 0)".to_owned(),
        ),
        ("th_x1".to_owned(), format!("toInt32({x} - {radius})")),
        (
            "th_y1".to_owned(),
            format!("toInt32(if(th_positive = 1, {y} + {radius}, {y} - {radius}))"),
        ),
        ("th_x2".to_owned(), format!("toInt32({x} + {radius})")),
        (
            "th_y2".to_owned(),
            format!("toInt32(if(th_positive = 1, {y} - {radius}, {y} + {radius}))"),
        ),
    ];
    let body = format!(
        "if({} = {}, -1, {})",
        side("th_x1", "th_y1"),
        side("th_x2", "th_y2"),
        super::super::fixed::intercept_vector(
            "toInt32(pt_x1)",
            "toInt32(pt_y1)",
            "toInt32(pt_dx)",
            "toInt32(pt_dy)",
            "th_x1",
            "th_y1",
            "toInt32(toInt64(th_x2) - toInt64(th_x1))",
            "toInt32(toInt64(th_y2) - toInt64(th_y1))",
        )
    );
    crate::sql::bind::chain_in("th", &values, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cells_run_one_column_at_a_time() {
        let text = cells("bbox", MAXRADIUS);
        assert!(text.starts_with("arrayFlatten(arrayMap(bx -> arrayMap(by ->"));
        assert!(text.contains("by * bmap_cols + bx"));
        assert!(text.contains("- bmap_orgx + -2097152"));
        assert!(text.contains("- bmap_orgy + 2097152"));
    }

    #[test]
    fn a_line_seen_twice_is_walked_once() {
        assert!(lines_in("cells").starts_with("arrayDistinct("));
    }

    /// The box test asks `P_PointOnLineSide` about two corners, not four.
    /// The engine's two sloping cases each evaluate it twice, and they
    /// take the same two corners with the x coordinates swapped.
    #[test]
    fn the_box_test_holds_two_point_tests() {
        let sql = box_on_line_side("bbox", "ld");
        // `P_PointOnLineSide` opens on the vertical line it answers for
        // without dividing, so the count of that test is the count of it.
        assert_eq!(sql.matches("line_dx[1 + ld] = 0").count(), 2, "{sql}");
        // The case picks the pair rather than the pair being written per
        // case, so the switch names the two cases that need no point test
        // and falls through for the two that do.
        assert_eq!(sql.matches("multiIf(line_slopetype").count(), 1, "{sql}");
        assert!(
            !sql.contains(&format!("line_slopetype[1 + ld] = {ST_POSITIVE}, (")),
            "{sql}"
        );
    }

    fn mobjs() -> Things<'static> {
        Things {
            m_x: "w_x",
            m_y: "w_y",
            m_radius: "w_radius",
            m_linkseq: "w_linkseq",
            alive: "w_alive",
        }
    }

    /// A block's things go in behind its own lines, so the two lists are
    /// concatenated per block rather than gathered separately.
    #[test]
    fn a_block_s_lines_go_in_ahead_of_its_things() {
        let sql = walked(Some(&mobjs()));
        let lines = sql.find("toUInt8(1)").expect("the lines");
        let things = sql.find("arraySort(k ->").expect("the things");
        assert!(lines < things, "{sql}");
    }

    /// A lines-only walk needs no interleaving, so it stays the one pass
    /// over the blocks that `P_BlockLinesIterator` is.
    #[test]
    fn a_lines_only_walk_holds_no_fold_over_the_blocks() {
        let sql = walked(None);
        assert!(sql.contains(&lines_in("pt_cells")), "{sql}");
        assert!(!sql.contains("arrayFold"), "{sql}");
    }

    #[test]
    fn every_builder_balances_its_parentheses() {
        let mut texts = vec![
            cells("bbox", 0),
            lines_in("cells"),
            box_on_line_side("bbox", "ld"),
            opening("ld", "fh", "ch"),
            finecosine("a"),
            finesine("a"),
            path_traverse("traces", None),
            path_traverse("traces", Some(&mobjs())),
        ];
        texts.extend(constants("nat").into_iter().map(|(_, expr)| expr));
        for text in texts {
            let depth = text.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{text}");
        }
    }
}
