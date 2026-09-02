//! Movement clipping, from `p_map.c`.
//!
//! `P_CheckPosition` asks whether a thing fits at a point and what floor
//! and ceiling it would stand between; `P_TryMove` asks that and then
//! moves it. Both are one expression over an array of pending moves, so a
//! tic that makes many of them evaluates the expression once and pays only
//! per move for the rest.

use crate::sql::{bind, bsp, fixed};

use super::maputl::{self, BOX_BOTTOM, BOX_LEFT, BOX_RIGHT, BOX_TOP, MAXRADIUS};

/// `p_mobj.h`
const MF_SPECIAL: i64 = 1;
const MF_SOLID: i64 = 2;
const MF_SHOOTABLE: i64 = 4;
const MF_NOCLIP: i64 = 0x1000;
const MF_TELEPORT: i64 = 0x8000;
const MF_MISSILE: i64 = 0x10000;
const MF_DROPOFF: i64 = 0x400;
const MF_FLOAT: i64 = 0x4000;
const MF_PICKUP: i64 = 0x800;

/// `doomdata.h`
const ML_BLOCKING: i64 = 1;
const ML_BLOCKMONSTERS: i64 = 2;

/// `p_map.c`: how far a thing steps up, and how far it stands over.
const MAXSTEP: i64 = 24 << 16;

/// Where each field of a pending move sits in its tuple.
pub mod ask {
    pub const SLOT: usize = 1;
    pub const X: usize = 2;
    pub const Y: usize = 3;
    pub const RADIUS: usize = 4;
    pub const HEIGHT: usize = 5;
    pub const Z: usize = 6;
    pub const FLAGS: usize = 7;
    pub const IS_PLAYER: usize = 8;
    /// How many fields a move carries.
    pub const WIDTH: usize = 8;
}

/// Where each field of the answer sits in its tuple.
pub mod answer {
    pub const OK: usize = 1;
    pub const FLOORZ: usize = 2;
    pub const CEILINGZ: usize = 3;
    pub const DROPOFFZ: usize = 4;
    pub const SUBSECTOR: usize = 5;
    pub const PICKED: usize = 6;
    pub const SPECHIT: usize = 7;
}

/// One pending move, as the tuple [`try_moves`] reads.
#[allow(clippy::too_many_arguments)]
pub fn asking(
    slot: &str,
    x: &str,
    y: &str,
    radius: &str,
    height: &str,
    z: &str,
    flags: &str,
    is_player: &str,
) -> String {
    format!(
        "(toUInt32({slot}), toInt32({x}), toInt32({y}), toInt32({radius}), \
         toInt32({height}), toInt32({z}), toInt32({flags}), toUInt8({is_player}))"
    )
}

/// An empty answer, for a move nobody made.
pub fn no_answer() -> String {
    "(toUInt8(0), toInt32(0), toInt32(0), toInt32(0), toInt32(0), \
     CAST([], 'Array(UInt32)'), CAST([], 'Array(Int32)'))"
        .to_owned()
}

/// The arrays a move reads: where every mobj is, and how high each sector
/// stands at this point in the tic.
pub struct World<'a> {
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_radius: &'a str,
    pub m_flags: &'a str,
    pub m_linkseq: &'a str,
    /// One per mobj slot: 1 while it is still on the list.
    pub alive: &'a str,
    pub floorheight: &'a str,
    pub ceilingheight: &'a str,
    pub line_special: &'a str,
}

/// `P_CheckPosition` and the tests `P_TryMove` adds, over every pending
/// move in `moves`.
///
/// The answer for each is `(ok, floorz, ceilingz, dropoffz, subsector,
/// picked, spechit)`: whether the move is allowed, the heights it would
/// stand between, the subsector it would stand in, the mobj slots it
/// touched that can be picked up, and the special lines it crossed.
pub fn try_moves(moves: &str, world: &World<'_>) -> String {
    let m = |field: usize| format!("mv.{field}");
    let (x, y) = (m(ask::X), m(ask::Y));
    let (radius, flags) = (m(ask::RADIUS), m(ask::FLAGS));
    let mut values: Vec<(String, String)> = Vec::new();
    let mut bind = |name: &str, expr: String| values.push((name.to_owned(), expr));

    bind(
        "tm_bbox",
        format!(
            "[toInt64({y}) + toInt64({radius}), toInt64({y}) - toInt64({radius}), \
             toInt64({x}) - toInt64({radius}), toInt64({x}) + toInt64({radius})]"
        ),
    );
    bind("tm_subsector", subsector(&x, &y));
    bind(
        "tm_sector",
        "toInt32(ssec_sector[1 + tm_subsector])".to_owned(),
    );
    bind(
        "tm_basefloor",
        format!("toInt32({}[1 + tm_sector])", world.floorheight),
    );
    bind(
        "tm_baseceiling",
        format!("toInt32({}[1 + tm_sector])", world.ceilingheight),
    );
    bind("tm_thing_cells", maputl::cells("tm_bbox", MAXRADIUS));
    bind("tm_line_cells", maputl::cells("tm_bbox", 0));
    bind("tm_thing_at", things_in("tm_thing_cells", world));
    bind("tm_line_at", maputl::lines_in("tm_line_cells"));
    bind(
        "tm_thing_touch",
        touching(&m(ask::SLOT), &x, &y, &radius, world),
    );
    bind("tm_line_hit", crossing(&flags, &m(ask::IS_PLAYER)));
    bind(
        "tm_thing_stop",
        format!(
            "toUInt32(indexOf(arrayMap(k -> toUInt8(bitAnd({}[k], {MF_SOLID}) != 0), \
             tm_thing_touch), 1))",
            world.m_flags
        ),
    );
    bind(
        "tm_line_stop",
        "toUInt32(indexOf(arrayMap(l -> l.2, tm_line_hit), toUInt8(1)))".to_owned(),
    );
    bind(
        "tm_picked",
        format!(
            "if(bitAnd({flags}, {MF_PICKUP}) = 0, CAST([], 'Array(UInt32)'), \
             arrayMap(k -> toUInt32(k), arrayFilter(k -> bitAnd({}[k], {MF_SPECIAL}) != 0, \
             if(tm_thing_stop = 0, tm_thing_touch, arraySlice(tm_thing_touch, 1, tm_thing_stop)))))",
            world.m_flags
        ),
    );
    // Only the lines the walk reached before something blocked it move
    // the floor and the ceiling or land on the special list.
    bind(
        "tm_line_open",
        "arrayMap(h -> h.1, if(tm_line_stop = 0, tm_line_hit, \
         arraySlice(tm_line_hit, 1, tm_line_stop - 1)))"
            .to_owned(),
    );
    bind(
        "tm_openings",
        format!(
            "arrayMap(l -> {}, tm_line_open)",
            maputl::opening("l", world.floorheight, world.ceilingheight)
        ),
    );
    bind(
        "tm_floorz",
        "toInt32(arrayMax(arrayPushBack(arrayMap(o -> o.2, tm_openings), \
         tm_basefloor)))"
            .to_owned(),
    );
    bind(
        "tm_ceilingz",
        "toInt32(arrayMin(arrayPushBack(arrayMap(o -> o.1, tm_openings), \
         tm_baseceiling)))"
            .to_owned(),
    );
    bind(
        "tm_dropoffz",
        "toInt32(arrayMin(arrayPushBack(arrayMap(o -> o.3, tm_openings), \
         tm_basefloor)))"
            .to_owned(),
    );
    let fits = format!(
        "bitAnd({flags}, {MF_NOCLIP}) != 0 OR (\
         toInt64(tm_ceilingz) - toInt64(tm_floorz) >= toInt64({height}) \
         AND (bitAnd({flags}, {MF_TELEPORT}) != 0 OR \
         toInt64(tm_ceilingz) - toInt64({z}) >= toInt64({height})) \
         AND (bitAnd({flags}, {MF_TELEPORT}) != 0 OR \
         toInt64(tm_floorz) - toInt64({z}) <= {MAXSTEP}) \
         AND (bitAnd({flags}, {}) != 0 OR \
         toInt64(tm_floorz) - toInt64(tm_dropoffz) <= {MAXSTEP}))",
        MF_DROPOFF | MF_FLOAT,
        height = m(ask::HEIGHT),
        z = m(ask::Z),
    );
    let body = format!(
        "(toUInt8(tm_thing_stop = 0 AND tm_line_stop = 0 AND ({fits})), \
         tm_floorz, tm_ceilingz, tm_dropoffz, tm_subsector, tm_picked, \
         arrayFilter(l -> {}[1 + l] != 0, tm_line_open))",
        world.line_special
    );
    format!("arrayMap(mv -> {}, {moves})", bind::chain(&values, &body))
}

/// `R_PointInSubsector` at a point.
fn subsector(x: &str, y: &str) -> String {
    let nodes = bsp::Nodes {
        x: "node_x",
        y: "node_y",
        dx: "node_dx",
        dy: "node_dy",
        child0: "node_child0",
        child1: "node_child1",
        count: "numnodes",
    };
    format!(
        "toInt32({})",
        bsp::point_in_subsector(x, y, &nodes, "bsp_depth")
    )
}

/// The mobjs filed under the cells the walk covers, in the order
/// `P_BlockThingsIterator` reaches them: one cell at a time, and inside a
/// cell the one linked last first.
fn things_in(cells: &str, world: &World<'_>) -> String {
    let cell = |slot: &str| {
        format!(
            "(bitShiftRight(toInt64({}[{slot}]) - bmap_orgy, {shift}) * bmap_cols + \
             bitShiftRight(toInt64({}[{slot}]) - bmap_orgx, {shift}))",
            world.m_y,
            world.m_x,
            shift = maputl::MAPBLOCKSHIFT
        )
    };
    format!(
        "arraySort(k -> (indexOf({cells}, {}), -toInt64({}[k])), \
         arrayFilter(k -> {alive}[k] = 1 AND has({cells}, {}), arrayEnumerate({alive})))",
        cell("k"),
        world.m_linkseq,
        cell("k"),
        alive = world.alive,
    )
}

/// `PIT_CheckThing` for a thing that is neither a missile nor a charging
/// skull: the ones the mover is close enough to touch.
fn touching(slot: &str, x: &str, y: &str, radius: &str, world: &World<'_>) -> String {
    let blockdist = format!("toInt64({}[k]) + toInt64({radius})", world.m_radius);
    format!(
        "arrayFilter(k -> k != {slot} \
         AND bitAnd({flags}[k], {}) != 0 \
         AND abs(toInt64({mx}[k]) - toInt64({x})) < {blockdist} \
         AND abs(toInt64({my}[k]) - toInt64({y})) < {blockdist}, tm_thing_at)",
        MF_SPECIAL | MF_SOLID | MF_SHOOTABLE,
        flags = world.m_flags,
        mx = world.m_x,
        my = world.m_y,
    )
}

/// `PIT_CheckLine` up to the point it decides: the lines the box actually
/// crosses, each with whether it blocks.
fn crossing(flags: &str, is_player: &str) -> String {
    let field = |name: &str| format!("{name}[1 + l]");
    let misses = format!(
        "tm_bbox[{BOX_RIGHT}] <= {} OR tm_bbox[{BOX_LEFT}] >= {} \
         OR tm_bbox[{BOX_TOP}] <= {} OR tm_bbox[{BOX_BOTTOM}] >= {}",
        field("line_left"),
        field("line_right"),
        field("line_bottom"),
        field("line_top"),
    );
    let blocks = format!(
        "{} = -1 OR (bitAnd({flags}, {MF_MISSILE}) = 0 AND (\
         bitAnd({}, {ML_BLOCKING}) != 0 OR \
         ({is_player} = 0 AND bitAnd({}, {ML_BLOCKMONSTERS}) != 0)))",
        field("line_side1"),
        field("line_flags"),
        field("line_flags"),
    );
    format!(
        "arrayMap(l -> (l, toUInt8({blocks})), \
         arrayFilter(l -> NOT ({misses}) AND {} = -1, tm_line_at))",
        maputl::box_on_line_side("tm_bbox", "l")
    )
}

/// `P_PointOnLineSide`, which `P_TryMove` uses to see whether a special
/// line was crossed.
pub fn point_on_line_side(x: &str, y: &str, line: &str) -> String {
    fixed::point_on_line_side(
        x,
        y,
        &format!("line_v1x[1 + {line}]"),
        &format!("line_v1y[1 + {line}]"),
        &format!("line_dx[1 + {line}]"),
        &format!("line_dy[1 + {line}]"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World<'static> {
        World {
            m_x: "w_x",
            m_y: "w_y",
            m_radius: "w_radius",
            m_flags: "w_flags",
            m_linkseq: "w_linkseq",
            alive: "w_alive",
            floorheight: "w_floor",
            ceilingheight: "w_ceiling",
            line_special: "w_special",
        }
    }

    #[test]
    fn the_primitive_appears_once_however_many_moves_it_is_given() {
        let sql = try_moves("pending", &world());
        assert_eq!(sql.matches("arrayMap(mv ->").count(), 1);
        assert_eq!(sql.matches("node_child0").count(), 1);
        assert_eq!(sql.matches("bmap_lines").count(), 1);
    }

    #[test]
    fn the_things_walk_is_widened_and_the_lines_walk_is_not() {
        let sql = try_moves("pending", &world());
        let cells: Vec<&str> = sql.match_indices("bmap_cols + bx").map(|_| "").collect();
        assert_eq!(cells.len(), 2, "one cell walk for things, one for lines");
        assert_eq!(
            sql.matches("2097152").count(),
            4,
            "the four sides of the widened box"
        );
    }

    #[test]
    fn the_expression_balances_its_parentheses() {
        let sql = try_moves("pending", &world());
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
    }
}
