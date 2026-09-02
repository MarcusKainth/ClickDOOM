//! Moving a sector's floor or ceiling, from `p_floor.c` and `p_map.c`.
//!
//! `T_MovePlane` is what every door, plat, floor and ceiling thinker calls.
//! It sets the height, asks `P_ChangeSector` whether everything still fits,
//! and puts the height back when something does not. What it answers,
//! `ok`, `crushed` or `pastdest`, is what the thinker above it branches on.

use crate::sql::bind;

use super::map::{self, World};
use super::maputl::{BOX_BOTTOM, BOX_LEFT, BOX_RIGHT, BOX_TOP, MAPBLOCKSHIFT, MAXRADIUS};

/// The sector tables a plane thinker reads, as constant arrays indexed by
/// sector plus one.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let ordered = |column: &str| {
        format!(
            "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((id, {column}))))\
             \n     FROM {db}.lv_sectors_static)"
        )
    };
    vec![
        ("sec_blockbox".to_owned(), ordered("blockbox")),
        ("sec_lines".to_owned(), ordered("lines")),
    ]
}

/// `P_FindLowestCeilingSurrounding`: the lowest ceiling of every sector
/// across a two sided line from this one, or `INT_MAX` when it has none.
pub fn lowest_ceiling_surrounding(sector: &str, ceilingheight: &str) -> String {
    let other = format!("if(line_front[1 + l] = {sector}, line_back[1 + l], line_front[1 + l])");
    format!(
        "arrayMin(arrayPushBack(arrayMap(l -> toInt64({ceilingheight}[1 + {other}]), \
         arrayFilter(l -> bitAnd(line_flags[1 + l], 4) != 0 AND ({other}) >= 0, \
         sec_lines[1 + {sector}])), toInt64(2147483647)))"
    )
}

/// `p_spec.h`: what `T_MovePlane` answers.
pub mod result {
    pub const OK: i64 = 0;
    pub const CRUSHED: i64 = 1;
    pub const PASTDEST: i64 = 2;
}

/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
const MF_DROPPED: i64 = 0x10000000;

/// Which plane a thinker moves, in `T_MovePlane`'s own numbering.
pub const FLOOR: i64 = 0;
pub const CEILING: i64 = 1;

/// The arrays `P_ChangeSector` reads and writes for every thing it clips.
pub struct Things<'a> {
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_radius: &'a str,
    pub m_height: &'a str,
    pub m_flags: &'a str,
    pub m_health: &'a str,
    pub alive: &'a str,
    /// The heights the clip works from and leaves behind.
    pub m_z: &'a str,
    pub m_floorz: &'a str,
    pub m_ceilingz: &'a str,
}

/// Where each part of `P_ChangeSector`'s answer sits.
pub mod clipped {
    /// One per sector asked about: 1 when something near it does not fit.
    pub const NOFIT: usize = 1;
    pub const Z: usize = 2;
    pub const FLOORZ: usize = 3;
    pub const CEILINGZ: usize = 4;
    /// 1 when a thing this cannot finish was in the way.
    pub const UNRESOLVED: usize = 5;
}

/// `P_ChangeSector` for every sector that moved this tic, in one clip.
///
/// The engine calls it once per `T_MovePlane`, each call walking the
/// sector's block box and re-clipping what it finds. The calls are
/// separate only where they reach the same thing: a thing near one moving
/// sector is clipped the same whether the others moved first or not,
/// because what it stands between comes from the lines around it and none
/// of them open onto a sector it is not near. So the clip is one batch
/// over every thing near any of them, and a thing near two leaves the tic
/// unresolved rather than being clipped for one and not the other.
///
/// A thing that no longer fits and is a corpse, a dropped item or
/// something unshootable also leaves the tic unresolved: the engine turns
/// it to gibs or takes it off the list, and neither happens here.
pub fn change_sector(
    sectors: &str,
    blockbox: &str,
    things: &Things<'_>,
    world: &World<'_>,
) -> String {
    let block = |side: usize, sign: &str, org: &str| {
        format!(
            "bitShiftRight(toInt64({blockbox}[1 + sec][{side}]) - {org} {sign} {MAXRADIUS}, \
             {MAPBLOCKSHIFT})"
        )
    };
    let cell = |axis: &str, org: &str| {
        format!("bitShiftRight(toInt64({axis}[k]) - {org}, {MAPBLOCKSHIFT})")
    };
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    // The slots standing on the blocks each sector covers, in slot order.
    value(
        "cs_near",
        format!(
            "arrayMap(sec -> arrayFilter(k -> {alive}[k] = 1 \
             AND {cx} >= greatest({left}, 0) AND {cx} <= least({right}, toInt64(bmap_cols) - 1) \
             AND {cy} >= greatest({bottom}, 0) AND {cy} <= least({top}, toInt64(bmap_rows) - 1), \
             arrayEnumerate({alive})), {sectors})",
            alive = things.alive,
            cx = cell(things.m_x, "bmap_orgx"),
            cy = cell(things.m_y, "bmap_orgy"),
            left = block(BOX_LEFT, "-", "bmap_orgx"),
            right = block(BOX_RIGHT, "+", "bmap_orgx"),
            bottom = block(BOX_BOTTOM, "-", "bmap_orgy"),
            top = block(BOX_TOP, "+", "bmap_orgy"),
        ),
    );
    value("cs_all", "arrayFlatten(cs_near)".to_owned());
    value("cs_slots", "arrayDistinct(cs_all)".to_owned());
    // `P_ThingHeightClip` runs `P_CheckPosition` where the thing already
    // stands, so the ask is the thing's own place.
    value(
        "cs_answers",
        map::heights(
            &format!(
                "arrayMap(k -> {}, cs_slots)",
                map::asking(
                    "k",
                    &format!("{}[k]", things.m_x),
                    &format!("{}[k]", things.m_y),
                    &format!("{}[k]", things.m_radius),
                    &format!("{}[k]", things.m_height),
                    &format!("{}[k]", things.m_z),
                    &format!("{}[k]", things.m_flags),
                    "0",
                )
            ),
            world,
        ),
    );
    let answered = |field: usize| format!("cs_answers[i].{field}");
    let slot = "cs_slots[i]";
    let height = format!("toInt64({}[{slot}])", things.m_height);
    let was = |array: &str| format!("toInt64({array}[{slot}])");
    value(
        "cs_each",
        format!(
            "arrayMap(i -> ({slot}, \
             toInt32(if({was_z} = {was_floorz}, {floorz}, \
             if({was_z} + {height} > {ceilingz}, {ceilingz} - {height}, {was_z}))), \
             toInt32({floorz}), toInt32({ceilingz}), \
             toUInt8({ceilingz} - {floorz} < {height})), \
             arrayEnumerate(cs_slots))",
            was_z = was(things.m_z),
            was_floorz = was(things.m_floorz),
            floorz = format!("toInt64({})", answered(map::height::FLOORZ)),
            ceilingz = format!("toInt64({})", answered(map::height::CEILINGZ)),
        ),
    );
    value(
        "cs_stuck",
        "arrayMap(c -> c.1, arrayFilter(c -> c.5 = 1, cs_each))".to_owned(),
    );
    // `PIT_ChangeSector` only reports a thing that is alive and can be
    // shot. The rest it turns to gibs or takes off the list.
    let shootable = format!(
        "bitAnd(toInt64({flags}[k]), {MF_SHOOTABLE}) != 0 AND toInt64({health}[k]) > 0 \
         AND bitAnd(toInt64({flags}[k]), {MF_DROPPED}) = 0",
        flags = things.m_flags,
        health = things.m_health,
    );
    // The things clipped are few and the list of things is long, so the
    // writes fold over the answers rather than mapping over the list. A
    // tic where nothing moved walks nothing at all.
    let put = |array: &str, member: usize| {
        format!(
            "arrayFold((acc, c) -> arrayMap((v, k) -> toInt32(if(k = c.1, c.{member}, v)), \
             acc, arrayEnumerate(acc)), cs_each, {array})"
        )
    };
    let body = format!(
        "(arrayMap(near -> toUInt8(arrayExists(k -> has(cs_stuck, k) AND ({shootable}), near)), \
         cs_near), {}, {}, {}, \
         toUInt8(length(cs_all) != length(cs_slots) \
         OR arrayExists(k -> NOT ({shootable}), cs_stuck)))",
        put(things.m_z, 2),
        put(things.m_floorz, 3),
        put(things.m_ceilingz, 4),
    );
    bind::chain_in("cs", &values, &body)
}

/// `T_MovePlane` for one thinker, as the values that build its answer.
///
/// The engine sets the height, clips, and puts the height back when the
/// clip reports something stuck. Which of those happens depends on the
/// plane and the direction, and the four cases do not agree: a floor going
/// down never returns early on a crush, a ceiling going up never reports
/// one, and every case that reaches its destination answers `pastdest`
/// even when it had to put the height back.
pub struct Plane<'a> {
    pub sector: &'a str,
    pub speed: &'a str,
    pub dest: &'a str,
    pub crush: &'a str,
    /// [`FLOOR`] or [`CEILING`].
    pub which: &'a str,
    pub direction: &'a str,
    pub height: &'a str,
}

/// Where each part of [`move_plane`]'s answer sits.
pub mod moved {
    /// [`super::result`]
    pub const RESULT: usize = 1;
    /// The height the sector is left at.
    pub const HEIGHT: usize = 2;
    /// 1 when the plane had to put the height back.
    pub const REVERTED: usize = 3;
}

/// What `T_MovePlane` answers and what height it leaves, given the answer
/// `P_ChangeSector` gives at the height it tried and at the one it came
/// from.
///
/// The height `T_MovePlane` sets before it clips: the destination on the
/// tic a step would pass it, and one step otherwise.
///
/// The caller clips at this height, because the engine sets it first and
/// `P_ChangeSector` reads the sector.
pub fn target(plane: &Plane<'_>) -> String {
    let last = format!("toInt64({})", plane.height);
    let speed = format!("toInt64({})", plane.speed);
    let dest = format!("toInt64({})", plane.dest);
    let up = format!("{} = 1", plane.direction);
    format!(
        "toInt64(if({}, {dest}, if({up}, {last} + {speed}, {last} - {speed})))",
        past(plane)
    )
}

/// Whether one more step would pass the destination.
fn past(plane: &Plane<'_>) -> String {
    let last = format!("toInt64({})", plane.height);
    let speed = format!("toInt64({})", plane.speed);
    let dest = format!("toInt64({})", plane.dest);
    format!(
        "(({d} = -1 AND {last} - {speed} < {dest}) OR ({d} = 1 AND {last} + {speed} > {dest}))",
        d = plane.direction
    )
}

/// `stuck` is what `P_ChangeSector` answered at `target`. The caller
/// evaluates both, because the clip is the expensive part and one
/// statement holds one of it, and applies the clip again at the old height
/// when the answer says the plane reverted.
pub fn move_plane(plane: &Plane<'_>, target: &str, stuck: &str) -> String {
    let last = format!("toInt64({})", plane.height);
    let up = format!("{} = 1", plane.direction);
    let down = format!("{} = -1", plane.direction);
    let floor = format!("{} = {FLOOR}", plane.which);
    let past = past(plane);
    let _ = &up;
    // A ceiling going up is the one case that never reports a crush.
    let reports = format!("NOT ({floor} = 0 AND {up})");
    // A floor going down is the one case that reverts before it answers.
    let keeps = format!("{} = 1 AND NOT ({floor} AND {down})", plane.crush);
    format!(
        "multiIf(\
         {past} AND {stuck} = 1, (toInt64({PASTDEST}), {last}, toUInt8(1)), \
         {past}, (toInt64({PASTDEST}), toInt64({target}), toUInt8(0)), \
         {stuck} = 1 AND {reports} AND {keeps}, \
         (toInt64({CRUSHED}), toInt64({target}), toUInt8(0)), \
         {stuck} = 1 AND {reports}, (toInt64({CRUSHED}), {last}, toUInt8(1)), \
         (toInt64({OK}), toInt64({target}), toUInt8(0)))",
        PASTDEST = result::PASTDEST,
        CRUSHED = result::CRUSHED,
        OK = result::OK,
    )
}
