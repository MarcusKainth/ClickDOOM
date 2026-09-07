//! Sector floors, from `p_floor.c`.
//!
//! A floor is a sector thinker that drives the sector's floor with
//! `T_MovePlane`. Reaching its destination removes it from the list;
//! `donutRaise` and `lowerAndChange` also copy a new special and texture
//! onto the sector then, which this does not implement.

/// `p_spec.h`: the `floor_e` values, in the order they are declared.
pub mod kind {
    pub const LOWER_FLOOR: i64 = 0;
    pub const LOWER_FLOOR_TO_LOWEST: i64 = 1;
    pub const TURBO_LOWER: i64 = 2;
    pub const RAISE_FLOOR: i64 = 3;
}

/// `p_spec.h`
pub const FLOORSPEED: i64 = 1 << 16;

/// `P_FindHighestFloorSurrounding`: the highest floor of a two sided
/// neighbor, or -500 map units when the sector has none. Unlike
/// `P_FindLowestFloorSurrounding`, the fold does not start from the
/// sector's own floor.
pub fn highest_floor_surrounding(sector: &str, floorheight: &str) -> String {
    let other = format!("if(line_front[1 + l] = {sector}, line_back[1 + l], line_front[1 + l])");
    format!(
        "arrayMax(arrayPushBack(arrayMap(l -> toInt64({floorheight}[1 + {other}]), \
         arrayFilter(l -> bitAnd(line_flags[1 + l], 4) != 0 AND ({other}) >= 0, \
         sec_lines[1 + {sector}])), toInt64({})))",
        -500 * (1i64 << 16)
    )
}
