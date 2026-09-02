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
        ("line_v1x".to_owned(), vertex(db, "x")),
        ("line_v1y".to_owned(), vertex(db, "y")),
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

/// A line's first vertex, which `P_BoxOnLineSide` compares a box against.
fn vertex(db: &str, axis: &str) -> String {
    format!(
        "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((l.id, v.{axis}))))\
         \n     FROM {db}.lv_lines AS l INNER JOIN {db}.lv_vertexes AS v ON v.id = l.v1)"
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
    let positive = format!(
        "({}, {})",
        side(at(BOX_LEFT), at(BOX_TOP)),
        side(at(BOX_RIGHT), at(BOX_BOTTOM))
    );
    let negative = format!(
        "({}, {})",
        side(at(BOX_RIGHT), at(BOX_TOP)),
        side(at(BOX_LEFT), at(BOX_BOTTOM))
    );
    // A horizontal line running left and a vertical line running down have
    // their sides the other way round.
    let flip = format!(
        "toUInt8(({} = {ST_HORIZONTAL} AND {dx} < 0) OR ({} = {ST_VERTICAL} AND {dy} < 0))",
        field("line_slopetype"),
        field("line_slopetype")
    );
    // The two corners land on the same side or they do not, and the flip
    // does not change whether they agree, so it is applied once at the end
    // rather than to each corner.
    let corner = |which: usize| {
        let (h, v, p, n) = (&horizontal, &vertical, &positive, &negative);
        format!(
            "toInt16(multiIf({slope} = {ST_HORIZONTAL}, ({h}).{which}, \
             {slope} = {ST_VERTICAL}, ({v}).{which}, \
             {slope} = {ST_POSITIVE}, ({p}).{which}, ({n}).{which}))",
            slope = field("line_slopetype")
        )
    };
    format!(
        "toInt32(multiIf(({} + {}) AS box_sides = 0, toInt16(bitXor(0, {flip})), \
         box_sides = 2, toInt16(bitXor(1, {flip})), toInt16(-1)))",
        corner(1),
        corner(2)
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

    #[test]
    fn every_builder_balances_its_parentheses() {
        let mut texts = vec![
            cells("bbox", 0),
            lines_in("cells"),
            box_on_line_side("bbox", "ld"),
            opening("ld", "fh", "ch"),
            finecosine("a"),
            finesine("a"),
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
