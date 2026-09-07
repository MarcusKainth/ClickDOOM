//! What a use press starts and what the plane thinkers then do.
//!
//! `P_UseSpecialLine` runs inside `P_PlayerThink`, so a door the press
//! makes is on the thinker list before `P_RunThinkers` reaches it and
//! moves on the same tic.

use clickdoom_spec::native_state::sector_thinker_kind as kind;

use super::State;
use super::doors::{self, Door, Opening};
use super::floor;
use super::map::World;
use super::plane::{self, Plane, Things};
use super::plats::{self, Plat};
use super::{mask, unresolved};
use crate::sql::bind;

/// `p_floor.c`: the `floor_e` values whose arrival changes the sector's
/// floor picture and its special, which this does not write.
///
/// `lowerAndChange` and `donutRaise`, counting from zero down the
/// declaration. The other two names with `Change` in them do their change
/// when the thinker is made, not when it arrives.
const CHANGES_TEXTURE: &str = "6, 11";

/// Every column the thinker list carries, in the order a new thinker
/// appends to them.
const THINKER_COLUMNS: [&str; 23] = [
    "s_seq",
    "s_kind",
    "s_sector",
    "s_type",
    "s_direction",
    "s_speed",
    "s_dest",
    "s_dest2",
    "s_count",
    "s_wait",
    "s_status",
    "s_oldstatus",
    "s_crush",
    "s_tag",
    "s_texture",
    "s_newspecial",
    "s_minlight",
    "s_maxlight",
    "s_mintime",
    "s_maxtime",
    "s_active",
    "s_activeplat_slot",
    "s_activeceil_slot",
];

/// `THINKER_COLUMNS`' element types, field for field, for the empty array
/// a spawn with nothing to append casts itself to.
const THINKER_TYPES: [&str; 23] = [
    "UInt32", "UInt8", "Int32", "Int32", "Int32", "Int32", "Int32", "Int32", "Int32", "Int32",
    "Int32", "Int32", "UInt8", "Int32", "Int32", "Int32", "Int32", "Int32", "Int32", "Int32",
    "UInt8", "Int32", "Int32",
];

/// `P_CrossSpecialLine`'s own switch (`p_spec.c`), every special it names,
/// TRIGGERS and RETRIGGERS together. A crossed line whose special is not
/// here does nothing, exactly as the switch falls through with no case
/// for it. [`PLAT_TRIGGER_SPECIALS`], [`DOOR_TRIGGER_SPECIALS`] and
/// [`FLOOR_TRIGGER_SPECIALS`] run; a crossing that reaches any other one
/// still leaves the tic unresolved rather than being guessed.
pub const CROSSABLE_SPECIALS: [i64; 72] = [
    2, 3, 4, 5, 6, 8, 10, 12, 13, 16, 17, 19, 22, 25, 30, 35, 36, 37, 38, 39, 40, 44, 52, 53, 54,
    56, 57, 58, 59, 72, 73, 74, 75, 76, 77, 79, 80, 81, 82, 83, 84, 86, 87, 88, 89, 90, 91, 92, 93,
    94, 95, 96, 97, 98, 100, 104, 105, 106, 107, 108, 109, 110, 119, 120, 121, 124, 125, 126, 128,
    129, 130, 141,
];

/// `P_CrossSpecialLine`'s non-player allow-list (`p_spec.c`): the only
/// specials a monster's crossing ever reaches the switch for at all.
/// Every other special returns before the switch runs, whether or not the
/// switch itself would have a case for it. None of [`DOOR_TRIGGER_SPECIALS`]
/// or [`FLOOR_TRIGGER_SPECIALS`] is here, so a monster's own crossing
/// never reaches a door or a floor.
pub const MONSTER_CROSSABLE_SPECIALS: [i64; 7] = [4, 10, 39, 88, 97, 125, 126];

/// `EV_DoPlat`'s downWaitUpStay: both `P_CrossSpecialLine` cases that spawn
/// it, one W1 (10, one-shot) and one WR (88, a retrigger).
pub const PLAT_TRIGGER_SPECIALS: [i64; 2] = [10, 88];

/// `EV_DoDoor`'s open and normal: `P_CrossSpecialLine` cases 2 (`vld_open`,
/// W1, one-shot) and 90 (`vld_normal`, WR, a retrigger).
pub const DOOR_TRIGGER_SPECIALS: [i64; 2] = [2, 90];

/// `EV_DoFloor`'s raiseFloor and turboLower: `P_CrossSpecialLine` cases 91
/// and 98, both WR (retriggers; neither clears the line's special).
pub const FLOOR_TRIGGER_SPECIALS: [i64; 2] = [91, 98];

/// `specials` minus [`PLAT_TRIGGER_SPECIALS`], [`DOOR_TRIGGER_SPECIALS`]
/// and [`FLOOR_TRIGGER_SPECIALS`]: what still leaves a crossing unresolved
/// once `cross_dispatch` runs the rest of them.
pub fn unhandled_crossable(specials: &[i64]) -> Vec<i64> {
    specials
        .iter()
        .copied()
        .filter(|special| {
            !PLAT_TRIGGER_SPECIALS.contains(special)
                && !DOOR_TRIGGER_SPECIALS.contains(special)
                && !FLOOR_TRIGGER_SPECIALS.contains(special)
        })
        .collect()
}

/// `P_FindSectorFromLineTag`: every sector whose tag matches `tag`, in
/// sector order, which is the order the engine's own linear scan finds
/// them in.
pub fn sectors_by_tag(tag: &str) -> String {
    format!("arrayFilter(sec -> sec_tag[sec] = ({tag}), arrayEnumerate(sec_tag))")
}

/// `specials` as a SQL `IN` list.
fn special_list(specials: &[i64]) -> String {
    specials
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether a landed move crosses a line whose special `specials` names,
/// by `P_TryMove`'s own test: the side flips between where the move
/// started and where it lands. `spechit` is walked last-added first,
/// matching `P_TryMove`'s `while (numspechit--)`.
#[allow(clippy::too_many_arguments)]
pub fn crosses_special(
    old_x: &str,
    old_y: &str,
    new_x: &str,
    new_y: &str,
    spechit: &str,
    line_special: &str,
    specials: &[i64],
) -> String {
    format!(
        "arrayExists(l -> {line_special}[1 + l] IN ({}) AND {} != {}, arrayReverse({spechit}))",
        special_list(specials),
        super::map::point_on_line_side(new_x, new_y, "l"),
        super::map::point_on_line_side(old_x, old_y, "l"),
    )
}

/// The last-added (`P_TryMove`'s own `while (numspechit--)` order) `spechit`
/// line whose special is one of `specials` and whose side flips between
/// where the move started and where it lands, or -1 if none does.
#[allow(clippy::too_many_arguments)]
pub fn crossed_line(
    old_x: &str,
    old_y: &str,
    new_x: &str,
    new_y: &str,
    spechit: &str,
    line_special: &str,
    specials: &[i64],
) -> String {
    format!(
        "arrayFold((acc, l) -> toInt64(if(acc = -1 AND {line_special}[1 + l] IN ({}) \
         AND {} != {}, l, acc)), arrayReverse({spechit}), toInt64(-1))",
        special_list(specials),
        super::map::point_on_line_side(new_x, new_y, "l"),
        super::map::point_on_line_side(old_x, old_y, "l"),
    )
}

/// How many `spechit` lines whose special is one of `specials` have a side
/// flip between where the move started and where it lands. [`crossed_line`]
/// only ever carries the first of these, so a caller reads this to tell a
/// move that crosses one from a move that crosses more than one.
#[allow(clippy::too_many_arguments)]
pub fn crossed_count(
    old_x: &str,
    old_y: &str,
    new_x: &str,
    new_y: &str,
    spechit: &str,
    line_special: &str,
    specials: &[i64],
) -> String {
    format!(
        "toInt64(arrayCount(l -> {line_special}[1 + l] IN ({}) AND {} != {}, {spechit}))",
        special_list(specials),
        super::map::point_on_line_side(new_x, new_y, "l"),
        super::map::point_on_line_side(old_x, old_y, "l"),
    )
}

/// One new thinker's fields, in `THINKER_COLUMNS`' order, as one tuple
/// [`spawn_planes`] appends.
pub fn new_plane(fields: &[String]) -> String {
    format!("({})", fields.join(", "))
}

/// An array of [`new_plane`] tuples, for a caller spawning none this tic.
pub fn no_planes() -> String {
    format!("CAST([], 'Array(Tuple({}))')", THINKER_TYPES.join(", "))
}

/// Appends one row per tuple `rows` carries onto every column
/// `THINKER_COLUMNS` names, in the order a new thinker occupies them.
///
/// `rows` is an array of [`new_plane`] tuples, one per sector an `EV_*`
/// spawns a thinker for this tic; [`no_planes`] costs the concat nothing.
/// `held` is the value each column carries before the append: `state.get`
/// for every column but one a caller has already changed on its own, the
/// way a use press turns an existing door's `s_direction` around instead
/// of appending to it.
fn spawn_planes(rows: &str, held: impl Fn(&str) -> String) -> Vec<(String, String)> {
    THINKER_COLUMNS
        .iter()
        .enumerate()
        .map(|(i, column)| {
            (
                format!("now_{column}"),
                format!(
                    "arrayConcat({}, arrayMap(t -> t.{}, {rows}))",
                    held(column),
                    i + 1
                ),
            )
        })
        .collect()
}

/// `P_UseSpecialLine` for the manual door specials, and the thinker it
/// appends.
///
/// Only the doors are here. A press that reaches any other special leaves
/// the tic unresolved, which is what `mv_useline` already does. `also` is
/// the caller's own `unresolved` mask so far, which this is the last stage
/// of `P_PlayerThink` to be able to add to.
pub fn use_special_line(state: &State, also: &str) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let line = "mv_useline";
    let opening = Opening {
        line,
        line_special: &s("line_special"),
        line_back: "line_back",
        sec_specialdata: &s("sec_specialdata"),
        sec_ceilingheight: &s("sec_ceilingheight"),
    };
    let sector = format!("toInt32(line_back[1 + {line}])");
    let lowest = plane::lowest_ceiling_surrounding(&sector, &s("sec_ceilingheight"));
    let mut bindings = vec![
        (
            "use_handles".to_owned(),
            format!(
                "toUInt8({line} >= 0 AND toInt64({}[1 + {line}]) \
                 IN (1, 26, 27, 28, 31, 32, 33, 34, 117, 118))",
                s("line_special")
            ),
        ),
        (
            "use_opened".to_owned(),
            format!(
                "if(use_handles = 1, {}, {})",
                doors::opening(&opening, &lowest),
                empty_opening(),
            ),
        ),
        (
            "use_makes".to_owned(),
            format!("toUInt8(use_opened.{} >= 0)", doors::opened::SECTOR),
        ),
        (
            "use_reopens".to_owned(),
            format!("toInt64(use_opened.{})", doors::opened::REOPENS),
        ),
    ];
    // The new thinker's fields, in the order `THINKER_COLUMNS` names them.
    let fields = [
        format!("toUInt32({})", s("next_seq")),
        format!("toUInt8({})", kind::DOOR),
        format!("toInt32(use_opened.{})", doors::opened::SECTOR),
        format!("toInt32(use_opened.{})", doors::opened::KIND),
        format!("toInt32(use_opened.{})", doors::opened::DIRECTION),
        format!("toInt32(use_opened.{})", doors::opened::SPEED),
        format!("toInt32(use_opened.{})", doors::opened::TOPHEIGHT),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        format!("toInt32({})", doors::VDOORWAIT),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toUInt8(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toUInt8(1)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
    ];
    let rows = format!(
        "if(use_makes = 1, [{}], {})",
        new_plane(&fields),
        no_planes()
    );
    // A press that turns an existing door around writes its direction
    // rather than appending to it.
    let held_direction = format!(
        "arrayMap((v, j) -> toInt32(if(use_reopens != 0 AND {}[j] = use_reopens, \
         if(v = -1, 1, -1), v)), {}, arrayEnumerate({}))",
        s("s_seq"),
        s("s_direction"),
        s("s_direction"),
    );
    bindings.extend(spawn_planes(&rows, |column| {
        if column == "s_direction" {
            held_direction.clone()
        } else {
            s(column)
        }
    }));
    bindings.extend([
        // `specialdata` names the thinker by its place on the list, which
        // is the slot the append just took.
        (
            "now_sec_specialdata".to_owned(),
            format!(
                "arrayMap((v, i) -> toUInt32(if(use_makes = 1 AND i = 1 + use_opened.{}, {}, v)), \
                 {held}, arrayEnumerate({held}))",
                doors::opened::SECTOR,
                format_args!("length({}) + 1", s("s_kind")),
                held = s("sec_specialdata"),
            ),
        ),
        (
            "now_line_special".to_owned(),
            format!(
                "arrayMap((v, i) -> toInt16(if(use_makes = 1 AND use_opened.{} = 1 \
                 AND i = 1 + {line}, 0, v)), {held}, arrayEnumerate({held}))",
                doors::opened::CLEARS,
                held = s("line_special"),
            ),
        ),
        // A door the press made took an identity. The writeback adds what
        // the tic spawned on top and names the column, because two stages
        // of one tic cannot both write it.
        (
            "use_next_seq".to_owned(),
            format!("toUInt32({} + if(use_makes = 1, 1, 0))", s("next_seq")),
        ),
        // A press that reaches a door is a tic this finishes. One that
        // reaches any other special, or a locked door, is not.
        // The writeback names the column, because the shots have their own
        // reason to leave the tic unresolved and one stage writes it once.
        (
            "use_unresolved".to_owned(),
            mask(
                also,
                &[
                    (unresolved::MV_UNFINISHED, "mv_unfinished = 1"),
                    (unresolved::PL_ACTION_NEEDED, "pl_action_needed = 1"),
                    (
                        unresolved::USE_UNHANDLED_SPECIAL,
                        &format!("({line} >= 0 AND use_handles = 0)"),
                    ),
                    (
                        unresolved::DOOR_OPEN_STUCK,
                        &format!("use_opened.{} = 1", doors::opened::UNRESOLVED),
                    ),
                ],
            ),
        ),
    ]);
    bindings
}

/// `EV_DoPlat`'s downWaitUpStay, `EV_DoDoor`'s open and normal, and
/// `EV_DoFloor`'s raiseFloor and turboLower, over every mover that crossed
/// a handled special this tic.
///
/// `P_FindSectorFromLineTag`'s walk runs once per tag a crossing named,
/// skipping a sector the matching `EV_*` finds already busy. Case 10 (W1)
/// and case 2 (W1) clear their line's special once their thinker spawns;
/// case 88, case 90, case 91 and case 98 (all WR) leave it, so a later
/// crossing can retrigger them once the sector frees up.
///
/// `px_crossed_line` and `tx_crossed_line` each name at most one line, the
/// first `P_TryMove`'s own spechit walk finds; a move whose spechit holds
/// two lines this dispatch would otherwise run leaves the tic unresolved
/// (`PX_MULTI_CROSSED`, `TX_MULTI_CROSSED`) rather than running the first
/// and dropping the second. `MONSTER_CROSSABLE_SPECIALS` names neither
/// `DOOR_TRIGGER_SPECIALS` nor `FLOOR_TRIGGER_SPECIALS`, so
/// `tx_crossed_line` never carries a door or a floor, and `cx_door_lines`
/// and `cx_floor_lines` below are each a single line or none.
///
/// The busy check reads `sec_specialdata` as `use_special_line` already
/// left it, since that stage runs first: a sector a press claims this tic
/// is never spawned into twice. A sector a press and a crossing both name
/// the same tic always resolves to the press, not to whichever order real
/// DOOM's own interleaving of the two would reach first.
pub fn cross_dispatch(state: &State) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let mut bindings: Vec<(String, String)> = Vec::new();
    let mut bind = |name: &str, expr: String| bindings.push((name.to_owned(), expr));
    // `line_special` is a genuine `native_state` column, so it is read
    // through `state` rather than by its own bare name: a bare reference
    // resolves to whatever this tic's own clearing below leaves it at,
    // not the value the crossing started the tic with.
    let ls = s("line_special");

    bind(
        "cx_lines",
        "arrayFilter(l -> l != -1, arrayConcat([px_crossed_line], tx_crossed_line))".to_owned(),
    );
    // `line_tag` is loaded fresh from `lv_lines` every tic rather than
    // carried in `native_state`, so it is read by its own name rather than
    // through `state`. `sectors_for` runs `P_FindSectorFromLineTag` once
    // per tag `lines` names, skipping a sector already busy.
    let sectors_for = |lines: &str| {
        format!(
            "arrayFilter(sec -> {}[sec] = 0, arrayDistinct(arrayFlatten(arrayMap(\
             tag -> {}, arrayDistinct(arrayMap(l -> toInt64(line_tag[1 + l]), {lines}))))))",
            s("sec_specialdata"),
            sectors_by_tag("tag"),
        )
    };

    bind(
        "cx_plat_lines",
        format!(
            "arrayFilter(l -> {ls}[1 + l] IN ({}), cx_lines)",
            special_list(&PLAT_TRIGGER_SPECIALS)
        ),
    );
    bind("cx_plat_sectors", sectors_for("cx_plat_lines"));
    bind(
        "cx_door_lines",
        format!(
            "arrayFilter(l -> {ls}[1 + l] IN ({}), cx_lines)",
            special_list(&DOOR_TRIGGER_SPECIALS)
        ),
    );
    // A sector `cx_plat_sectors` already claimed this tic is not claimed
    // again, in the unlikely case a door tag and a plat tag name the same
    // sector.
    bind(
        "cx_door_sectors",
        format!(
            "arrayFilter(sec -> indexOf(cx_plat_sectors, sec) = 0, {})",
            sectors_for("cx_door_lines")
        ),
    );

    let plat_floor = format!("toInt64({}[sec])", s("sec_floorheight"));
    let plat_low = plane::lowest_floor_surrounding("(sec - 1)", &s("sec_floorheight"));
    // The new plat's fields, in the order `THINKER_COLUMNS` names them.
    let plat_fields = [
        format!("toUInt32({} + i - 1)", s("next_seq")),
        format!("toUInt8({})", kind::PLAT),
        "toInt32(sec - 1)".to_owned(),
        format!("toInt32({})", plats::kind::DOWN_WAIT_UP_STAY),
        "toInt32(0)".to_owned(),
        format!("toInt32({})", plats::PLATSPEED * 4),
        format!("toInt32(least({plat_low}, {plat_floor}))"),
        format!("toInt32({plat_floor})"),
        "toInt32(0)".to_owned(),
        format!("toInt32({})", plats::TICRATE * plats::PLATWAIT),
        format!("toInt32({})", plats::status::DOWN),
        "toInt32(0)".to_owned(),
        "toUInt8(0)".to_owned(),
        // `sec_tag` is loaded fresh from `lv_sectors_static` every tic
        // rather than carried in `native_state`, so it is read by its own
        // name rather than through `state`.
        "toInt32(sec_tag[sec])".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toUInt8(1)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
    ];
    bind(
        "cx_plat_rows",
        format!(
            "arrayMap((sec, i) -> {}, cx_plat_sectors, arrayEnumerate(cx_plat_sectors))",
            new_plane(&plat_fields)
        ),
    );

    bind(
        "cx_door_type",
        format!(
            "toInt64(if(empty(cx_door_lines), 0, \
             if({ls}[1 + cx_door_lines[1]] = 2, {}, {})))",
            doors::kind::OPEN,
            doors::kind::NORMAL,
        ),
    );
    let door_top = doors::topheight("(sec - 1)", &s("sec_ceilingheight"));
    // The new door's fields, in the order `THINKER_COLUMNS` names them.
    let door_fields = [
        format!(
            "toUInt32({} + length(cx_plat_sectors) + i - 1)",
            s("next_seq")
        ),
        format!("toUInt8({})", kind::DOOR),
        "toInt32(sec - 1)".to_owned(),
        "toInt32(cx_door_type)".to_owned(),
        "toInt32(1)".to_owned(),
        format!("toInt32({})", doors::VDOORSPEED),
        format!("toInt32({door_top})"),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        format!("toInt32({})", doors::VDOORWAIT),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toUInt8(0)".to_owned(),
        "toInt32(sec_tag[sec])".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toUInt8(1)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
    ];
    bind(
        "cx_door_rows",
        format!(
            "arrayMap((sec, i) -> {}, cx_door_sectors, arrayEnumerate(cx_door_sectors))",
            new_plane(&door_fields)
        ),
    );

    bind(
        "cx_floor_lines",
        format!(
            "arrayFilter(l -> {ls}[1 + l] IN ({}), cx_lines)",
            special_list(&FLOOR_TRIGGER_SPECIALS)
        ),
    );
    // A sector either of the other two kinds already claimed this tic is
    // not claimed again.
    bind(
        "cx_floor_sectors",
        format!(
            "arrayFilter(sec -> indexOf(cx_plat_sectors, sec) = 0 \
             AND indexOf(cx_door_sectors, sec) = 0, {})",
            sectors_for("cx_floor_lines")
        ),
    );

    // Case 91 (raiseFloor) and case 98 (turboLower) are the only two
    // `FLOOR_TRIGGER_SPECIALS` names, and `px_crossed_line` carries at
    // most one line, so `cx_floor_lines` is a single line or none, the
    // same as `cx_door_lines`.
    bind(
        "cx_floor_special",
        format!("toInt64(if(empty(cx_floor_lines), 0, {ls}[1 + cx_floor_lines[1]]))"),
    );
    let raise_ceiling = plane::lowest_ceiling_surrounding("(sec - 1)", &s("sec_ceilingheight"));
    let turbo_floor = floor::highest_floor_surrounding("(sec - 1)", &s("sec_floorheight"));
    let floor_own_floor = format!("toInt64({}[sec])", s("sec_floorheight"));
    let floor_own_ceiling = format!("toInt64({}[sec])", s("sec_ceilingheight"));
    let floor_dest = format!(
        "toInt64(multiIf(\
         cx_floor_special = 91, least(toInt64({raise_ceiling}), {floor_own_ceiling}), \
         cx_floor_special = 98 AND toInt64({turbo_floor}) != {floor_own_floor}, \
         toInt64({turbo_floor}) + {eight}, \
         toInt64({turbo_floor})))",
        eight = 8 * (1i64 << 16),
    );
    // The new floor's fields, in the order `THINKER_COLUMNS` names them.
    let floor_fields = [
        format!(
            "toUInt32({} + length(cx_plat_sectors) + length(cx_door_sectors) + i - 1)",
            s("next_seq")
        ),
        format!("toUInt8({})", kind::FLOOR),
        "toInt32(sec - 1)".to_owned(),
        format!(
            "toInt32(if(cx_floor_special = 98, {}, {}))",
            floor::kind::TURBO_LOWER,
            floor::kind::RAISE_FLOOR
        ),
        "toInt32(if(cx_floor_special = 98, -1, 1))".to_owned(),
        format!(
            "toInt32(if(cx_floor_special = 98, {}, {}))",
            floor::FLOORSPEED * 4,
            floor::FLOORSPEED
        ),
        format!("toInt32({floor_dest})"),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toUInt8(0)".to_owned(),
        "toInt32(sec_tag[sec])".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
        "toUInt8(1)".to_owned(),
        "toInt32(0)".to_owned(),
        "toInt32(0)".to_owned(),
    ];
    bind(
        "cx_floor_rows",
        format!(
            "arrayMap((sec, i) -> {}, cx_floor_sectors, arrayEnumerate(cx_floor_sectors))",
            new_plane(&floor_fields)
        ),
    );

    bind(
        "cx_rows",
        "arrayConcat(cx_plat_rows, cx_door_rows, cx_floor_rows)".to_owned(),
    );
    bindings.extend(spawn_planes("cx_rows", |column| s(column)));
    // `specialdata` names the thinker by its place on the list, which is
    // the slot the append just took: the plat rows first, then the door
    // rows, then the floor rows, in the order `cx_rows` concatenates them.
    let base = format!("length({})", s("s_kind"));
    bindings.push((
        "now_sec_specialdata".to_owned(),
        format!(
            "arrayMap((v, i) -> toUInt32(multiIf(\
             indexOf(cx_plat_sectors, i) != 0, {base} + indexOf(cx_plat_sectors, i), \
             indexOf(cx_door_sectors, i) != 0, \
             {base} + length(cx_plat_sectors) + indexOf(cx_door_sectors, i), \
             indexOf(cx_floor_sectors, i) != 0, \
             {base} + length(cx_plat_sectors) + length(cx_door_sectors) \
             + indexOf(cx_floor_sectors, i), \
             v)), {held}, arrayEnumerate({held}))",
            held = s("sec_specialdata"),
        ),
    ));
    bindings.push((
        "now_line_special".to_owned(),
        format!(
            "arrayMap((v, i) -> toInt16(if(v IN (10, 2) AND has(cx_lines, i - 1), 0, v)), \
             {held}, arrayEnumerate({held}))",
            held = s("line_special"),
        ),
    ));
    bindings
}

/// A press that starts nothing.
fn empty_opening() -> String {
    "(toInt32(-1), toInt64(0), toInt64(0), toInt64(0), toInt64(0), toUInt8(0), \
     toInt64(0), toUInt8(0))"
        .to_owned()
}

/// `T_VerticalDoor` over every door on the thinker list.
///
/// `P_RunThinkers` walks the list once and each thinker reads the world
/// the one before it left. A plane thinker only ever writes the sector
/// `specialdata` gave it, so two of them read and write nothing in
/// common and the order between them cannot be seen. That makes this a
/// map rather than a fold, and two thinkers on one sector leave the tic
/// unresolved rather than being run in a made up order.
///
/// The clip every one of them needs is the expensive part, so it happens
/// once for all of them.
pub fn planes(state: &State) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let at = |name: &str| format!("{}[j]", s(name));
    let sector = "toInt32(plane_sector[j])";

    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value(
        "plane_sector",
        format!(
            "arrayMap(j -> toInt32({}), arrayEnumerate({}))",
            at("s_sector"),
            s("s_kind")
        ),
    );
    // A door moves while its direction says so; a plat moves while its
    // status is one of the two that run.
    value(
        "plane_runs",
        format!(
            "arrayMap(j -> toUInt8({active} = 1 AND (\
             ({k} = {DOOR} AND {dir} != 0) OR \
             ({k} = {FLOOR} AND {dir} != 0) OR \
             ({k} = {PLAT} AND {st} IN ({UP}, {DOWN})))), arrayEnumerate({kinds}))",
            active = at("s_active"),
            k = at("s_kind"),
            dir = at("s_direction"),
            st = at("s_status"),
            DOOR = kind::DOOR,
            FLOOR = kind::FLOOR,
            PLAT = kind::PLAT,
            UP = plats::status::UP,
            DOWN = plats::status::DOWN,
            kinds = s("s_kind"),
        ),
    );
    // A door going down closes on the floor it stands over, and one going
    // up stops at the height it was given.
    let door_plane = door_plane(sector, "plane_height[j]");
    for (name, column) in [
        ("plane_speed", "s_speed"),
        ("plane_count", "s_count"),
        ("plane_type", "s_type"),
        ("plane_wait", "s_wait"),
        ("plane_status", "s_status"),
        ("plane_crush", "s_crush"),
        ("plane_low", "s_dest"),
        ("plane_high", "s_dest2"),
    ] {
        value(
            name,
            format!(
                "arrayMap(j -> toInt64({}), arrayEnumerate({}))",
                at(column),
                s("s_kind")
            ),
        );
    }
    // A plat runs up to its high and down to its low. A door going down
    // closes on the floor it stands over, and one going up stops at the
    // height it was given.
    value(
        "plane_direction",
        format!(
            "arrayMap(j -> toInt64(if({k} = {PLAT}, \
             if(plane_status[j] = {UP}, 1, -1), toInt64({dir}))), arrayEnumerate({kinds}))",
            k = at("s_kind"),
            PLAT = kind::PLAT,
            UP = plats::status::UP,
            dir = at("s_direction"),
            kinds = s("s_kind"),
        ),
    );
    value(
        "plane_dest",
        format!(
            "arrayMap(j -> toInt64(multiIf(\
             {k} = {PLAT}, if(plane_status[j] = {UP}, plane_high[j], plane_low[j]), \
             {k} = {DOOR} AND plane_direction[j] = -1, toInt64({}[1 + {sector}]), \
             toInt64({}))), arrayEnumerate({kinds}))",
            s("sec_floorheight"),
            at("s_dest"),
            k = at("s_kind"),
            DOOR = kind::DOOR,
            PLAT = kind::PLAT,
            UP = plats::status::UP,
            kinds = s("s_kind"),
        ),
    );
    // A door and a ceiling drive the ceiling; a plat and a floor drive the
    // floor. `T_MovePlane` numbers them that way round.
    value(
        "plane_which",
        format!(
            "arrayMap(j -> toInt64(if({} IN ({}, {}), {}, {})), arrayEnumerate({k}))",
            at("s_kind"),
            kind::DOOR,
            kind::CEILING,
            plane::CEILING,
            plane::FLOOR,
            k = s("s_kind"),
        ),
    );
    value(
        "plane_height",
        format!(
            "arrayMap(j -> toInt64(if(plane_which[j] = {}, {}[1 + {sector}], {}[1 + {sector}])), \
             arrayEnumerate({}))",
            plane::CEILING,
            s("sec_ceilingheight"),
            s("sec_floorheight"),
            s("s_kind")
        ),
    );
    value(
        "plane_target",
        format!(
            "arrayMap(j -> {}, arrayEnumerate({}))",
            plane::target(&door_plane),
            s("s_kind")
        ),
    );
    // The sectors that move, in list order, and the clip they all share.
    // Both walks start from `plane_act`, the slots the pass was entered
    // for.
    value(
        "plane_moving",
        "arrayMap(j -> toInt32(plane_sector[j]), \
         arrayFilter(j -> plane_runs[j] = 1, plane_act))"
            .to_owned(),
    );
    // The sectors that move are few and there are many, so the height goes
    // in by a fold over the thinkers that moved one.
    value(
        "plane_running",
        "arrayFilter(j -> plane_runs[j] = 1, plane_act)".to_owned(),
    );
    let trying = |which: i64, from: &str| {
        format!(
            "arrayFold((acc, j) -> arrayMap((v, i) -> toInt32(if(i = 1 + plane_sector[j] \
             AND plane_which[j] = {which}, plane_target[j], v)), acc, arrayEnumerate(acc)), \
             plane_running, {from})"
        )
    };
    value(
        "plane_trying",
        trying(plane::CEILING, &s("sec_ceilingheight")),
    );
    value(
        "plane_trying_floor",
        trying(plane::FLOOR, &s("sec_floorheight")),
    );
    let world = World {
        m_x: &s("m_x"),
        m_y: &s("m_y"),
        m_radius: &s("m_radius"),
        m_flags: &s("m_flags"),
        m_linkseq: &s("m_linkseq"),
        alive: "plane_alive",
        floorheight: "plane_trying_floor",
        ceilingheight: "plane_trying",
        line_special: &s("line_special"),
    };
    let things = Things {
        m_x: &s("m_x"),
        m_y: &s("m_y"),
        m_radius: &s("m_radius"),
        m_height: &s("m_height"),
        m_flags: &s("m_flags"),
        m_health: &s("m_health"),
        alive: "plane_alive",
        m_z: &s("m_z"),
        m_floorz: &s("m_floorz"),
        m_ceilingz: &s("m_ceilingz"),
    };
    // The player's stage compacts the list before this one runs, so every
    // slot it leaves is alive.
    value(
        "plane_alive",
        format!("arrayMap(v -> toUInt8(1), {})", s("m_x")),
    );
    // The clip is the body of a fold over the sectors that moved, which
    // holds one entry or none. A thinker that is on the list and moves
    // nothing leaves every height where it found it, and the fold's
    // starting value is exactly that, so a tic where a plat counts its
    // wait down or a door counts its stay down pays nothing for the clip.
    // The walk starts from `clip_act`, because a lambda body that reads
    // neither of its parameters is evaluated outside the lambda.
    value(
        "plane_clip",
        format!(
            "arrayFold((clip_at, clip_act) -> {}, \
             arrayFilter(a -> notEmpty(a), [plane_moving]), \
             (CAST([], 'Array(UInt8)'), {}, {}, {}, toUInt8(0)))",
            plane::change_sector("clip_act", "sec_blockbox", &things, &world),
            s("m_z"),
            s("m_floorz"),
            s("m_ceilingz"),
        ),
    );
    // Each thinker reads the answer for its own sector.
    value(
        "plane_stuck",
        format!(
            "arrayMap(j -> toUInt8(if(plane_runs[j] = 0, 0, \
             (plane_clip.{})[indexOf(plane_moving, toInt32(plane_sector[j]))])), arrayEnumerate({}))",
            plane::clipped::NOFIT,
            s("s_kind")
        ),
    );
    value(
        "plane_moved",
        format!(
            "arrayMap(j -> {}, arrayEnumerate({}))",
            plane::move_plane(&door_plane, "plane_target[j]", "plane_stuck[j]"),
            s("s_kind")
        ),
    );
    let door = Door {
        kind: "plane_type[j]",
        direction: "plane_door_direction[j]",
        count: "plane_count[j]",
        wait: "plane_wait[j]",
    };
    value(
        "plane_door_direction",
        format!(
            "arrayMap(j -> toInt64({}), arrayEnumerate({}))",
            at("s_direction"),
            s("s_kind")
        ),
    );
    value(
        "plane_door",
        format!(
            "arrayMap(j -> {}, arrayEnumerate({}))",
            doors::vertical_door(&door, "plane_moved[j]"),
            s("s_kind")
        ),
    );
    // Two thinkers driving one sector would have to be run in order, and
    // the order is what this shape gives up.
    value(
        "plane_shared",
        "toUInt8(length(plane_moving) != length(arrayDistinct(plane_moving)))".to_owned(),
    );
    let plat = Plat {
        kind: "plane_type[j]",
        status: "plane_status[j]",
        count: "plane_count[j]",
        wait: "plane_wait[j]",
        crush: "plane_crush[j]",
        low: "plane_low[j]",
        floorheight: "plane_moved[j].2",
    };
    value(
        "plane_plat",
        format!(
            "arrayMap(j -> {}, arrayEnumerate({}))",
            plats::plat_raise(&plat, "plane_moved[j]"),
            s("s_kind")
        ),
    );
    value(
        "plane_is_plat",
        format!(
            "arrayMap(j -> toUInt8({} = {}), arrayEnumerate({}))",
            at("s_kind"),
            kind::PLAT,
            s("s_kind")
        ),
    );
    value(
        "plane_is_door",
        format!(
            "arrayMap(j -> toUInt8({} = {}), arrayEnumerate({}))",
            at("s_kind"),
            kind::DOOR,
            s("s_kind")
        ),
    );
    // A plat that is only waiting still runs its count down, and a door
    // that is only waiting at the top still runs its own down too, so
    // either acts on tics the plane pass does not move anything for it.
    value(
        "plane_ticks",
        format!(
            "arrayMap(j -> toUInt8({} = 1 AND ({} IN ({}, {}) OR plane_runs[j] = 1)), \
             arrayEnumerate({k}))",
            at("s_active"),
            at("s_kind"),
            kind::PLAT,
            kind::DOOR,
            k = s("s_kind"),
        ),
    );
    value(
        "plane_done",
        format!(
            "arrayMap(j -> toUInt8(multiIf(plane_is_plat[j] = 1, \
             plane_ticks[j] = 1 AND plane_plat[j].{} = 1, \
             {k} = {FLOOR}, plane_runs[j] = 1 AND plane_moved[j].{} = {PASTDEST}, \
             plane_runs[j] = 1 AND plane_door[j].{} = 1)), arrayEnumerate({kinds}))",
            plats::ran::DONE,
            plane::moved::RESULT,
            doors::ran::DONE,
            k = at("s_kind"),
            FLOOR = kind::FLOOR,
            PASTDEST = plane::result::PASTDEST,
            kinds = s("s_kind")
        ),
    );

    // Everything the pass leaves comes back in one tuple, because the
    // chain that computes it holds the clip and one statement holds one of
    // those.
    let each = |body: String| format!("arrayMap(j -> {body}, arrayEnumerate({}))", s("s_kind"));
    let scatter = |which: i64, from: &str| {
        format!(
            "arrayFold((acc, j) -> arrayMap((v, i) -> toInt32(if(i = 1 + plane_sector[j] \
             AND plane_which[j] = {which}, plane_moved[j].{}, v)), acc, arrayEnumerate(acc)), \
             plane_running, {from})",
            plane::moved::HEIGHT,
        )
    };
    let ran = |member: usize, column: &str| {
        each(format!(
            "toInt32(if(plane_is_door[j] = 1 AND plane_ticks[j] = 1, plane_door[j].{member}, {}))",
            at(column)
        ))
    };
    let keep = each("toUInt8(if(plane_done[j] = 1, 0, 1))".to_owned());
    let ceiling = scatter(plane::CEILING, &s("sec_ceilingheight"));
    let floor = scatter(plane::FLOOR, &s("sec_floorheight"));
    let direction = ran(doors::ran::DIRECTION, "s_direction");
    let kind_now = ran(doors::ran::KIND, "s_type");
    // The count belongs to whichever thinker ran or ticked, and the
    // status is the plat's alone.
    let count = each(format!(
        "toInt32(multiIf(plane_is_plat[j] = 1 AND plane_ticks[j] = 1, plane_plat[j].{}, \
         plane_is_plat[j] = 1, {c}, \
         plane_is_door[j] = 1 AND plane_ticks[j] = 1, plane_door[j].{}, {c}))",
        plats::ran::COUNT,
        doors::ran::COUNT,
        c = at("s_count"),
    ));
    let status = each(format!(
        "toInt32(if(plane_is_plat[j] = 1 AND plane_ticks[j] = 1, plane_plat[j].{}, {}))",
        plats::ran::STATUS,
        at("s_status"),
    ));
    let z = format!("plane_clip.{}", plane::clipped::Z);
    let floorz = format!("plane_clip.{}", plane::clipped::FLOORZ);
    let ceilingz = format!("plane_clip.{}", plane::clipped::CEILINGZ);
    let specialdata = format!(
        "arrayFold((acc, j) -> arrayMap((v, i) -> \
         toUInt32(if(i = 1 + plane_sector[j], 0, v)), acc, arrayEnumerate(acc)), \
         arrayFilter(j -> plane_done[j] = 1, arrayEnumerate({k})), {d})",
        k = s("s_kind"),
        d = s("sec_specialdata"),
    );
    // A plane that crushes keeps moving into what is stuck, which the
    // clip here does not do, so a running thinker with crush set leaves
    // the tic unresolved.
    let plane_unresolved = mask(
        &s("unresolved"),
        &[
            (unresolved::PLANE_SHARED, "plane_shared = 1"),
            (
                unresolved::PLANE_CLIP_STUCK,
                &format!("plane_clip.{} = 1", plane::clipped::UNRESOLVED),
            ),
            (
                unresolved::PLANE_FLOOR_CHANGER,
                &format!(
                    "arrayExists(j -> plane_done[j] = 1 AND {k2} = {FLOOR2} \
                     AND plane_type[j] IN ({CHANGERS}), arrayEnumerate({k}))",
                    k2 = at("s_kind"),
                    FLOOR2 = kind::FLOOR,
                    CHANGERS = CHANGES_TEXTURE,
                    k = s("s_kind"),
                ),
            ),
            (
                unresolved::PLANE_REVERTED,
                &format!(
                    "arrayExists(j -> plane_runs[j] = 1 AND plane_moved[j].{} = 1, \
                     arrayEnumerate({k}))",
                    plane::moved::REVERTED,
                    k = s("s_kind"),
                ),
            ),
            (
                unresolved::DOOR_RUN_STUCK,
                &format!(
                    "arrayExists(j -> plane_runs[j] = 1 AND plane_door[j].{} = 1, \
                     arrayEnumerate({k}))",
                    doors::ran::UNRESOLVED,
                    k = s("s_kind"),
                ),
            ),
            (
                unresolved::PLANE_CRUSH,
                &format!(
                    "arrayExists(j -> plane_runs[j] = 1 AND plane_crush[j] = 1, \
                     arrayEnumerate({k}))",
                    k = s("s_kind"),
                ),
            ),
        ],
    );
    // Each member of the pass's answer, as what it computes and what the
    // same field holds on a tic the pass does not run. The two are written
    // together because the second is the fold's starting value and has to
    // agree with the first member for member. `held` reads them back by
    // position.
    let answer: Vec<(String, String)> = vec![
        (
            keep,
            format!("arrayMap(j -> toUInt8(1), arrayEnumerate({}))", s("s_kind")),
        ),
        (ceiling, s("sec_ceilingheight")),
        (direction, s("s_direction")),
        (count, s("s_count")),
        (kind_now, s("s_type")),
        (z, s("m_z")),
        (floorz, s("m_floorz")),
        (ceilingz, s("m_ceilingz")),
        (specialdata, s("sec_specialdata")),
        (plane_unresolved, format!("toUInt64({})", s("unresolved"))),
        (floor, s("sec_floorheight")),
        (status, s("s_status")),
    ];
    let tuple = |member: fn(&(String, String)) -> &String| {
        format!(
            "({})",
            answer
                .iter()
                .map(member)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    // The values go through one chain rather than becoming bindings of
    // their own. A binding read more than once inside a SELECT is expanded
    // at each place that reads it, and the clip is too big to write out
    // five times.
    //
    // The chain is the body of a fold over `acting`, which holds one entry
    // on a tic whose thinker list carries a plane thinker and none on a
    // tic that does not, so a tic that runs none of them costs the list
    // walk alone. Both of the pass's walks start from `plane_act`, because
    // a lambda body that reads neither of its parameters is evaluated
    // outside the lambda and the fold then skips nothing.
    let held = |field: usize| format!("planes.{field}");
    let mut bindings = vec![
        (
            "planes".to_owned(),
            format!(
                "arrayFold((plane_at, plane_act) -> {}, {}, {})",
                bind::chain_in("plane", &values, &tuple(|a| &a.0)),
                acting(state),
                tuple(|a| &a.1),
            ),
        ),
        ("kept".to_owned(), held(1)),
        (
            "thinker_slot".to_owned(),
            "arrayMap((a, c) -> toUInt32(if(a = 1, c, 0)), kept, arrayCumSum(kept))".to_owned(),
        ),
        ("now_sec_ceilingheight".to_owned(), held(2)),
        ("now_sec_floorheight".to_owned(), held(11)),
        ("now_m_z".to_owned(), held(6)),
        ("now_m_floorz".to_owned(), held(7)),
        ("now_m_ceilingz".to_owned(), held(8)),
        ("now_unresolved".to_owned(), held(10)),
    ];
    // A thinker `P_RemoveThinker` marked comes off the list, and the slots
    // after it move down. `specialdata` names a thinker by its slot, so it
    // moves with them.
    for column in THINKER_COLUMNS {
        let from = match column {
            "s_direction" => held(3),
            "s_count" => held(4),
            "s_type" => held(5),
            "s_status" => held(12),
            _ => s(column),
        };
        bindings.push((
            format!("now_{column}"),
            format!("arrayFilter((v, a) -> a = 1, {from}, kept)"),
        ));
    }
    bindings.push((
        "now_sec_specialdata".to_owned(),
        format!(
            "arrayMap(v -> toUInt32(if(v = 0, 0, thinker_slot[v])), {})",
            held(9)
        ),
    ));
    bindings
}

/// The slots the pass has anything to do for, as a list holding one entry
/// or none.
///
/// `P_RunThinkers` reaches `T_VerticalDoor`, `T_PlatRaise` and
/// `T_MoveFloor` only for a thinker of one of those kinds whose function
/// is on the list, so a tic whose list carries none of them leaves every
/// field the pass writes alone. The entry is the whole slot list, because
/// the pass answers for all of them at once.
fn acting(state: &State) -> String {
    let s = |column: &str| state.get(column);
    format!(
        "arrayFilter(a -> notEmpty(a), [arrayFilter(j -> {active}[j] = 1 \
         AND {k}[j] IN ({DOOR}, {PLAT}, {FLOOR}, {CEILING}), arrayEnumerate({k}))])",
        active = s("s_active"),
        k = s("s_kind"),
        DOOR = kind::DOOR,
        PLAT = kind::PLAT,
        FLOOR = kind::FLOOR,
        CEILING = kind::CEILING,
    )
}

/// The plane a door drives: the sector's ceiling, at the speed and in the
/// direction the thinker carries.
fn door_plane<'a>(sector: &'a str, height: &'a str) -> Plane<'a> {
    Plane {
        sector,
        speed: "toInt64(plane_speed[j])",
        dest: "plane_dest[j]",
        crush: "0",
        which: "plane_which[j]",
        direction: "plane_direction[j]",
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass() -> String {
        planes(&State::default())
            .into_iter()
            .find(|(name, _)| name == "planes")
            .map(|(_, expr)| expr)
            .expect("the pass is one binding")
    }

    /// `line_special` is a `native_state` column, not a fresh-per-tic
    /// constant, so a dispatch filter has to read it through `state`
    /// rather than by its own bare name. A bare reference in one of these
    /// bindings would resolve to whatever `now_line_special`'s own
    /// clearing below leaves the line at, which is invisible for a
    /// retriggerable special but drops a one-shot special dispatched and
    /// cleared in the same tic.
    #[test]
    fn every_dispatch_filter_reads_line_special_through_state() {
        let bindings = cross_dispatch(&State::default());
        for name in [
            "cx_plat_lines",
            "cx_door_lines",
            "cx_floor_lines",
            "cx_door_type",
            "cx_floor_special",
        ] {
            let (_, expr) = bindings
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("{name} is one of cross_dispatch's own bindings"));
            let bare = expr.match_indices("line_special").any(|(at, _)| {
                !expr[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            });
            assert!(!bare, "{name}: {expr}");
            assert!(expr.contains("prev_line_special"), "{name}: {expr}");
        }
    }

    #[test]
    fn sectors_by_tag_filters_sec_tag_by_the_tag_given() {
        let sql = sectors_by_tag("line_tag[1 + l]");
        assert!(sql.contains("sec_tag[sec] = (line_tag[1 + l])"), "{sql}");
        assert!(sql.contains("arrayEnumerate(sec_tag)"), "{sql}");
    }

    /// `cross_dispatch` runs every special `PLAT_TRIGGER_SPECIALS` and
    /// `DOOR_TRIGGER_SPECIALS` name, so a crossing unresolved bit built
    /// from `unhandled_crossable` no longer names any of them, and names
    /// nothing else it did not already.
    #[test]
    fn unhandled_crossable_takes_out_exactly_the_dispatched_triggers() {
        let unhandled = unhandled_crossable(&CROSSABLE_SPECIALS);
        for special in PLAT_TRIGGER_SPECIALS
            .into_iter()
            .chain(DOOR_TRIGGER_SPECIALS)
            .chain(FLOOR_TRIGGER_SPECIALS)
        {
            assert!(!unhandled.contains(&special), "{special}");
        }
        for special in CROSSABLE_SPECIALS {
            if !PLAT_TRIGGER_SPECIALS.contains(&special)
                && !DOOR_TRIGGER_SPECIALS.contains(&special)
                && !FLOOR_TRIGGER_SPECIALS.contains(&special)
            {
                assert!(unhandled.contains(&special), "{special}");
            }
        }
    }

    /// `crossed_count` answers how many lines match, not whether one does,
    /// which is what tells a move that crosses two plat triggers from one
    /// that crosses one: `crossed_line` alone cannot.
    #[test]
    fn crossed_count_counts_rather_than_asks_whether_any_match() {
        let sql = crossed_count("ox", "oy", "nx", "ny", "hits", "line_special", &[10, 88]);
        assert!(sql.contains("arrayCount("), "{sql}");
        assert!(sql.contains("IN (10, 88)"), "{sql}");
    }

    /// `CROSSABLE_SPECIALS` names no special twice; `p_spec.c`'s own
    /// switch does not either, checked by reading `case N:` out of the
    /// vendored source for `P_CrossSpecialLine`'s TRIGGERS and RETRIGGERS
    /// blocks together (lines 549 to 960).
    #[test]
    fn every_crossable_special_is_named_once() {
        let mut sorted = CROSSABLE_SPECIALS.to_vec();
        sorted.sort_unstable();
        let mut unique = sorted.clone();
        unique.dedup();
        assert_eq!(sorted.len(), unique.len());
    }

    /// 31 is `EV_VerticalDoor`'s manual "open" special, not one
    /// `P_CrossSpecialLine`'s switch names anywhere (`p_spec.c`, both
    /// blocks): a demo3 player who brushes a type-31 line's box without
    /// truly crossing it, or a monster who does cross it, reaches no
    /// case and does nothing, which is why this leaves neither list.
    #[test]
    fn a_manual_only_special_crosses_neither_list() {
        assert!(!CROSSABLE_SPECIALS.contains(&31));
        assert!(!MONSTER_CROSSABLE_SPECIALS.contains(&31));
    }

    /// 88 (PlatDownWaitUp, retriggerable) is the one special on both
    /// lists: `P_CrossSpecialLine`'s own non-player pre-check names it
    /// (`p_spec.c` lines 530-542) and its main switch's RETRIGGERS block
    /// also has a case for it, so a monster reaches the same dispatch a
    /// player does.
    #[test]
    fn the_monster_triggered_plat_is_on_both_lists() {
        assert!(CROSSABLE_SPECIALS.contains(&88));
        assert!(MONSTER_CROSSABLE_SPECIALS.contains(&88));
    }

    /// `p_spec.c`'s non-player pre-check (lines 530-542) names exactly
    /// these seven: two teleport triggers, their monster-only pair, one
    /// door and one plat, both by their WR/retrigger number.
    #[test]
    fn the_monster_allow_list_is_the_seven_p_spec_c_names() {
        let mut sorted = MONSTER_CROSSABLE_SPECIALS;
        sorted.sort_unstable();
        assert_eq!(sorted, [4, 10, 39, 88, 97, 125, 126]);
    }

    /// Every special the monster allow-list names is also one
    /// `P_CrossSpecialLine`'s main switch has a case for, since that is
    /// the switch a monster reaches once the pre-check passes it through.
    #[test]
    fn the_monster_allow_list_is_a_subset_of_the_full_switch() {
        for special in MONSTER_CROSSABLE_SPECIALS {
            assert!(
                CROSSABLE_SPECIALS.contains(&special),
                "{special} is not one of P_CrossSpecialLine's own cases"
            );
        }
    }

    #[test]
    fn no_planes_casts_to_an_array_of_one_tuple_per_thinker_column() {
        let sql = no_planes();
        assert!(sql.starts_with("CAST([], 'Array(Tuple("), "{sql}");
        let types = sql
            .trim_start_matches("CAST([], 'Array(Tuple(")
            .trim_end_matches("))')")
            .split(", ")
            .count();
        assert_eq!(types, THINKER_COLUMNS.len(), "{sql}");
    }

    #[test]
    fn spawn_planes_concats_one_row_per_column_in_order() {
        let rows = "my_rows";
        let bindings = spawn_planes(rows, |column| format!("held_{column}"));
        assert_eq!(bindings.len(), THINKER_COLUMNS.len());
        for (i, (column, expr)) in bindings.iter().enumerate() {
            assert_eq!(*column, format!("now_{}", THINKER_COLUMNS[i]));
            assert_eq!(
                *expr,
                format!(
                    "arrayConcat(held_{}, arrayMap(t -> t.{}, {rows}))",
                    THINKER_COLUMNS[i],
                    i + 1
                )
            );
        }
    }

    /// A caller that has already changed one column's held value ahead of
    /// the append, the way a use press turns a door's `s_direction`
    /// around, reads back through the same override rather than the
    /// column's own state.
    #[test]
    fn spawn_planes_reads_an_overridden_column_through_the_override() {
        let bindings = spawn_planes("my_rows", |column| {
            if column == "s_direction" {
                "turned".to_owned()
            } else {
                format!("held_{column}")
            }
        });
        let direction = bindings
            .iter()
            .find(|(name, _)| name == "now_s_direction")
            .map(|(_, expr)| expr.clone())
            .expect("s_direction is one of the columns");
        assert!(direction.starts_with("arrayConcat(turned, "), "{direction}");
    }

    #[test]
    fn the_pass_is_one_fold_over_the_slots_it_runs_for() {
        let sql = pass();
        assert_eq!(sql.matches("arrayFold((plane_at, plane_act) ->").count(), 1);
        assert!(sql.contains(&format!("IN ({}, {}, ", kind::DOOR, kind::PLAT)));
    }

    /// A thinker that is on the list and moves nothing leaves every height
    /// where it found it, so the clip runs for a tic that moves a sector
    /// rather than for every tic that carries a plane thinker.
    #[test]
    fn the_clip_is_one_fold_over_the_sectors_that_moved() {
        let sql = pass();
        assert_eq!(sql.matches("arrayFold((clip_at, clip_act) ->").count(), 1);
        assert_eq!(sql.matches("arrayMap(clip ->").count(), 1);
        // One list holds the slots the pass runs for and one the sectors
        // they move, and each is what its fold walks.
        assert_eq!(sql.matches("arrayFilter(a -> notEmpty(a), [").count(), 2);
    }

    /// A lambda body that reads neither of its parameters is evaluated
    /// outside the lambda, so the fold skips the clip only while both of
    /// the pass's walks start from the slots it hands them.
    #[test]
    fn both_walks_start_from_the_folds_parameter() {
        assert_eq!(pass().matches("= 1, plane_act)").count(), 2);
    }

    /// The fold's starting value stands in for the answer, so it has to
    /// carry one member per member of it.
    #[test]
    fn what_the_pass_leaves_has_a_member_for_each_one_it_writes() {
        let sql = pass();
        let arguments = split(
            sql.strip_suffix(')')
                .expect("a call")
                .split_once('(')
                .expect("a call")
                .1,
        );
        let [_lambda, _acting, start] = arguments.as_slice() else {
            panic!("the fold takes three arguments, not {}", arguments.len())
        };
        let members = split(
            start
                .strip_prefix('(')
                .and_then(|t| t.strip_suffix(')'))
                .expect("a tuple"),
        );
        assert_eq!(members.len(), 12);
    }

    /// `text` cut at the commas outside every bracket.
    fn split(text: &str) -> Vec<String> {
        let mut parts = vec![String::new()];
        let mut depth = 0;
        for c in text.chars() {
            match c {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(String::new());
                    continue;
                }
                _ => {}
            }
            parts.last_mut().expect("a part").push(c);
        }
        parts.into_iter().map(|p| p.trim().to_owned()).collect()
    }
}
