//! What a use press starts and what the plane thinkers then do.
//!
//! `P_UseSpecialLine` runs inside `P_PlayerThink`, so a door the press
//! makes is on the thinker list before `P_RunThinkers` reaches it and
//! moves on the same tic.

use clickdoom_spec::native_state::sector_thinker_kind as kind;

use super::State;
use super::doors::{self, Door, Opening};
use super::map::World;
use super::plane::{self, Plane, Things};
use super::plats::{self, Plat};
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

/// `P_UseSpecialLine` for the manual door specials, and the thinker it
/// appends.
///
/// Only the doors are here. A press that reaches any other special leaves
/// the tic unresolved, which is what `mv_useline` already does.
pub fn use_special_line(state: &State) -> Vec<(String, String)> {
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
    let appended: Vec<String> = vec![
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
    for (column, value) in THINKER_COLUMNS.iter().zip(&appended) {
        let held = s(column);
        // A press that turns an existing door around writes its direction
        // rather than appending anything.
        let held = if *column == "s_direction" {
            format!(
                "arrayMap((v, j) -> toInt32(if(use_reopens != 0 AND {}[j] = use_reopens, \
                 if(v = -1, 1, -1), v)), {held}, arrayEnumerate({held}))",
                s("s_seq")
            )
        } else {
            held
        };
        bindings.push((
            format!("now_{column}"),
            format!("if(use_makes = 1, arrayPushBack({held}, {value}), {held})"),
        ));
    }
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
        (
            "now_next_seq".to_owned(),
            format!("toUInt32({} + if(use_makes = 1, 1, 0))", s("next_seq")),
        ),
        // A press that reaches a door is a tic this finishes. One that
        // reaches any other special, or a locked door, is not.
        (
            "now_unresolved".to_owned(),
            format!(
                "toUInt8(mv_unfinished = 1 OR pl_action_needed = 1 \
                 OR ({line} >= 0 AND use_handles = 0) OR use_opened.{} = 1)",
                doors::opened::UNRESOLVED
            ),
        ),
    ]);
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
    // A plat that is only waiting still runs its count down, so it acts on
    // tics the plane pass does not move anything for it.
    value(
        "plane_ticks",
        format!(
            "arrayMap(j -> toUInt8({} = 1 AND ({} = {} OR plane_runs[j] = 1)), \
             arrayEnumerate({k}))",
            at("s_active"),
            at("s_kind"),
            kind::PLAT,
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
            "toInt32(if(plane_runs[j] = 1, plane_door[j].{member}, {}))",
            at(column)
        ))
    };
    let keep = each("toUInt8(if(plane_done[j] = 1, 0, 1))".to_owned());
    let ceiling = scatter(plane::CEILING, &s("sec_ceilingheight"));
    let floor = scatter(plane::FLOOR, &s("sec_floorheight"));
    let direction = ran(doors::ran::DIRECTION, "s_direction");
    let kind_now = ran(doors::ran::KIND, "s_type");
    // The count belongs to whichever thinker ran, and the status is the
    // plat's alone.
    let count = each(format!(
        "toInt32(multiIf(plane_is_plat[j] = 1 AND plane_ticks[j] = 1, plane_plat[j].{}, \
         plane_is_plat[j] = 1, {c}, \
         plane_runs[j] = 1, plane_door[j].{}, {c}))",
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
    let unresolved = format!(
        "toUInt8({} = 1 OR plane_shared = 1 OR plane_clip.{} = 1 \
         OR arrayExists(j -> plane_done[j] = 1 AND {k2} = {FLOOR2} \
         AND plane_type[j] IN ({CHANGERS}), arrayEnumerate({k})) \
         OR arrayExists(j -> plane_runs[j] = 1 AND (plane_moved[j].{} = 1 OR plane_door[j].{} = 1 \
         OR plane_crush[j] = 1), arrayEnumerate({k})))",
        s("unresolved"),
        plane::clipped::UNRESOLVED,
        plane::moved::REVERTED,
        doors::ran::UNRESOLVED,
        k2 = at("s_kind"),
        FLOOR2 = kind::FLOOR,
        CHANGERS = CHANGES_TEXTURE,
        k = s("s_kind"),
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
        (unresolved, format!("toUInt8({})", s("unresolved"))),
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
