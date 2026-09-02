//! The state row a level starts from.
//!
//! `G_InitNew` clears the random index and `P_SetupLevel` builds the world.
//! `P_LoadThings` calls `P_SpawnMapThing` once per map thing in lump order,
//! and `P_SpawnSpecials` walks the sectors in order giving each light
//! special its thinker. Both draw from `P_Random`, and the draw count of a
//! thing is fixed by its type, so the whole spawn is one pass over the
//! things with a running total of the draws before each.
//!
//! The row this writes is tic 0: the world before the first tic runs.

use clickdoom_spec::native_state::sector_thinker_kind as sector_kind;

use crate::sql::{Statement, bsp};

use super::unimplemented;

/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `tables.h`
const ANG45: i64 = 0x2000_0000;
/// `p_local.h`
const VIEWHEIGHT: i64 = 41 * FRACUNIT;
/// `p_pspr.c`
const WEAPONBOTTOM: i64 = 128 * FRACUNIT;
/// `p_pspr.c`
const RAISESPEED: i64 = 6 * FRACUNIT;
/// `doomdef.h`
const MTF_AMBUSH: i32 = 8;
/// `doomdef.h`: `wp_pistol`, and the marker for no pending change.
const WP_PISTOL: i32 = 1;
const WP_NOCHANGE: i32 = 10;
/// `p_mobj.h`
const MF_AMBUSH: i32 = 32;
const MF_SPAWNCEILING: i32 = 256;
const MF_COUNTKILL: i32 = 0x40_0000;
const MF_COUNTITEM: i32 = 0x80_0000;
/// `doomdata.h`
const ML_TWOSIDED: i32 = 4;
/// `deh_misc.h`
const INITIAL_HEALTH: i32 = 100;
const INITIAL_BULLETS: i32 = 50;
/// `p_inter.c`
const MAXAMMO: [i64; 4] = [200, 50, 300, 50];
/// `p_spec.h`
const STROBEBRIGHT: i32 = 5;
const FASTDARK: i32 = 15;
const SLOWDARK: i32 = 35;
/// `p_lights.c`: what `P_SpawnLightFlash` sets before it draws its count.
const FLASH_MINTIME: i32 = 7;
const FLASH_MAXTIME: i32 = 64;
/// `p_lights.c`: `P_SpawnFireFlicker`
const FLICKER_COUNT: i32 = 4;
const FLICKER_LIGHT_MARGIN: i32 = 16;
/// `m_menu.c`: what `M_Init` leaves the skull animation at.
const SKULL_ANIM_COUNTER: i32 = 10;
/// `p_spec.h`
const MAXBUTTONS: usize = 16;
/// `st_stuff.c`: `ST_updateFaceWidget` starts with no attack seen.
const NO_ATTACK_DOWN: i32 = -1;
/// `st_stuff.c`: `ST_initData` and the pain-offset cache's own start.
const NO_OLD_HEALTH: i32 = -1;
const NO_PALETTE: i32 = -1;

/// Every statement the level's first state row needs: the guards
/// `P_SetupLevel` would call `I_Error` from, then the row.
pub fn statements(db: &str) -> Vec<Statement> {
    let mut statements = guards(db);
    statements.push(Statement::sql(state_row(db)));
    statements
}

/// What the engine stops on rather than spawning a wrong world.
fn guards(db: &str) -> Vec<Statement> {
    let unknown_type = format!(
        "SELECT throwIf(count() > 0, \
         'P_SpawnMapThing: a thing names a type mobjinfo does not carry')\n\
         FROM {db}.lv_things\n\
         WHERE {} = 2 AND type NOT IN (SELECT doomednum FROM {db}.mobjinfo)",
        thing_kind("type", "options", &skill_bit(db))
    );
    let one_player = format!(
        "SELECT throwIf(count() != 1, \
         'P_LoadThings: the map does not hold one player one start')\n\
         FROM {db}.lv_things WHERE type = 1"
    );
    // `P_BringUpWeapon` puts the ready weapon's up state on the weapon
    // sprite, and `P_SetPsprite` runs that state's action on entry. The
    // row below applies `A_Raise` and stops there, which holds only while
    // the state raises, sets no coordinate and has a tic to wait.
    let raises = format!(
        "SELECT throwIf(count() != 1, \
         'P_BringUpWeapon: the ready weapon does not raise')\n\
         FROM {db}.states\n\
         WHERE id = (SELECT upstate FROM {db}.weaponinfo WHERE id = {WP_PISTOL})\n\
         AND tics > 0 AND misc1 = 0\n\
         AND action = (SELECT id FROM {db}.action_functions WHERE name = 'A_Raise')"
    );
    [unknown_type, one_player, raises]
        .into_iter()
        .map(Statement::sql)
        .collect()
}

fn state_row(db: &str) -> String {
    super::insert(db, &constants(db), &row(db), &sources(db))
}

// ---------------------------------------------------------------------------
// The engine's tables, as constant arrays indexed by id plus one
// ---------------------------------------------------------------------------

fn constants(db: &str) -> Vec<(&'static str, String)> {
    let column = |table: &str, column: &str| table_column(db, table, column);
    vec![
        ("rnd", column("rndtable", "value")),
        ("state_tics", column("states", "tics")),
        ("state_sprite", column("states", "sprite")),
        ("state_frame", column("states", "frame")),
        ("node_x", column("lv_nodes", "x")),
        ("node_y", column("lv_nodes", "y")),
        ("node_dx", column("lv_nodes", "dx")),
        ("node_dy", column("lv_nodes", "dy")),
        ("node_child0", column("lv_nodes", "children[1]")),
        ("node_child1", column("lv_nodes", "children[2]")),
        ("numnodes", format!("(SELECT count() FROM {db}.lv_nodes)")),
        (
            "bsp_depth",
            format!("(SELECT max(depth) FROM {db}.lv_ssec_path)"),
        ),
        ("ssec_sector", column("lv_subsectors", "sector")),
        (
            "sector_floorheight",
            column("lv_sectors_static", "floorheight"),
        ),
        (
            "sector_ceilingheight",
            column("lv_sectors_static", "ceilingheight"),
        ),
        (
            "sector_lightlevel",
            column("lv_sectors_static", "lightlevel"),
        ),
        ("line_flags", column("lv_lines", "flags")),
        ("line_front", column("lv_lines", "sector0")),
        ("line_back", column("lv_lines", "sector1")),
        ("skill_bit", skill_bit(db)),
        (
            "up_state",
            format!("(SELECT upstate FROM {db}.weaponinfo WHERE id = {WP_PISTOL})"),
        ),
    ]
}

/// One table column as an array indexed by `id` plus one. The sort is
/// explicit because an aggregate reads its input in whatever order the
/// pipeline hands it over.
fn table_column(db: &str, table: &str, column: &str) -> String {
    format!(
        "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((id, {column})))) \
         FROM {db}.{table})"
    )
}

/// `P_SpawnMapThing`'s skill mask: bit 0 on the two easiest skills, bit 2
/// on nightmare, and one bit per skill in between.
fn skill_bit(db: &str) -> String {
    let skill = format!("(SELECT skill FROM {db}.demo_header)");
    format!("toInt32(multiIf({skill} = 0, 1, {skill} = 4, 4, bitShiftLeft(1, {skill} - 1)))")
}

// ---------------------------------------------------------------------------
// P_LoadThings and P_SpawnMapThing
// ---------------------------------------------------------------------------

/// What a map thing spawns: 0 nothing, 1 the console player, 2 a map thing.
///
/// The tests come in `P_SpawnMapThing`'s order. A deathmatch start and a
/// player start other than the console player's are counted and left, and
/// the multiplayer-only bit and the skill bit filter the rest.
fn thing_kind(kind: &str, options: &str, skill_bit: &str) -> String {
    format!(
        "toUInt8(multiIf(\
         {kind} = 11, 0, \
         {kind} <= 0, 0, \
         {kind} = 1, 1, \
         {kind} <= 4, 0, \
         bitAnd({options}, 16) != 0, 0, \
         bitAnd({options}, {skill_bit}) = 0, 0, \
         2))"
    )
}

/// One row per map thing: what it spawns, which `mobjinfo` it is, and the
/// number of the first `P_Random` call the spawn makes.
///
/// `P_SpawnMobj` draws once for `lastlook`, and `P_SpawnMapThing` draws
/// again to scatter the first state's tics when that state has any. Both
/// run in thing order, so a running total over the things gives every
/// spawn its call numbers without simulating the sequence.
fn thing_rows(db: &str) -> String {
    let types = format!(
        "(\n        SELECT toInt16(doomednum) AS doomednum, toInt32(min(id) + 1) AS found\n        \
         FROM {db}.mobjinfo WHERE doomednum > 0 GROUP BY doomednum\n    )"
    );
    let classified = format!(
        "SELECT\n    \
         t.id AS thing,\n    \
         t.x AS thing_x,\n    \
         t.y AS thing_y,\n    \
         t.angle AS thing_angle,\n    \
         t.type AS thing_type,\n    \
         t.options AS thing_options,\n    \
         {} AS kind,\n    \
         toInt32(if(kind = 1, 0, d.found - 1)) AS info\n\
         FROM {db}.lv_things AS t\n\
         LEFT JOIN {types} AS d ON d.doomednum = t.type",
        thing_kind("t.type", "t.options", "skill_bit")
    );
    let with_info = format!(
        "SELECT\n    \
         c.*,\n    \
         mi.spawnstate AS spawnstate,\n    \
         mi.spawnhealth AS spawnhealth,\n    \
         mi.reactiontime AS reactiontime,\n    \
         mi.radius AS radius,\n    \
         mi.height AS height,\n    \
         mi.flags AS flags,\n    \
         toUInt32(multiIf(c.kind = 1, 1, c.kind = 2, \
         if(state_tics[1 + mi.spawnstate] > 0, 2, 1), 0)) AS draws\n\
         FROM\n(\n{}\n) AS c\n\
         LEFT JOIN {db}.mobjinfo AS mi ON mi.id = toUInt32(greatest(c.info, 0))",
        indent(&classified)
    );
    format!(
        "SELECT\n    \
         *,\n    \
         toUInt32(sum(draws) OVER \
         (ORDER BY thing ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) \
         - draws + 1) AS first\n\
         FROM\n(\n{}\n)",
        indent(&with_info)
    )
}

/// `P_SpawnMobj` and the rest of `P_SpawnMapThing`, one row per mobj in
/// thinker order.
fn mobj_rows(db: &str) -> String {
    let nodes = bsp::Nodes {
        x: "node_x",
        y: "node_y",
        dx: "node_dx",
        dy: "node_dy",
        child0: "node_child0",
        child1: "node_child1",
        count: "numnodes",
    };
    let spawnpoint = |field: &str| format!("toInt16(if(kind = 2, {field}, 0))");
    let fields = [
        ("thing", "thing".to_owned()),
        ("kind", "kind".to_owned()),
        ("draws", "draws".to_owned()),
        (
            "x",
            "toInt32(bitShiftLeft(toInt64(thing_x), 16))".to_owned(),
        ),
        (
            "y",
            "toInt32(bitShiftLeft(toInt64(thing_y), 16))".to_owned(),
        ),
        (
            "angle",
            format!(
                "toUInt32(bitAnd(toInt64({ANG45}) * \
                 toInt64(intDiv(toInt32(thing_angle), 45)), 4294967295))"
            ),
        ),
        ("type", "toInt32(info)".to_owned()),
        ("state", "toInt32(spawnstate)".to_owned()),
        ("sprite", "toInt32(state_sprite[1 + state])".to_owned()),
        ("frame", "toInt32(state_frame[1 + state])".to_owned()),
        (
            "tics",
            "toInt32(if(kind = 2 AND state_tics[1 + state] > 0, \
             1 + toInt32(rnd[1 + bitAnd(first + 1, 255)]) % state_tics[1 + state], \
             state_tics[1 + state]))"
                .to_owned(),
        ),
        (
            "lastlook",
            "toInt32(rnd[1 + bitAnd(first, 255)] % 4)".to_owned(),
        ),
        ("radius", "toInt32(radius)".to_owned()),
        ("height", "toInt32(height)".to_owned()),
        (
            "flags",
            format!(
                "toInt32(bitOr(flags, if(kind = 2 AND \
                 bitAnd(toInt32(thing_options), {MTF_AMBUSH}) != 0, {MF_AMBUSH}, 0)))"
            ),
        ),
        (
            "health",
            format!("toInt32(if(kind = 1, {INITIAL_HEALTH}, spawnhealth))"),
        ),
        ("reactiontime", "toInt32(reactiontime)".to_owned()),
        ("player", "toInt8(if(kind = 1, 0, -1))".to_owned()),
        (
            "subsector",
            format!(
                "toInt32({})",
                bsp::point_in_subsector("x", "y", &nodes, "bsp_depth")
            ),
        ),
        ("sector", "toInt32(ssec_sector[1 + subsector])".to_owned()),
        (
            "floorz",
            "toInt32(sector_floorheight[1 + sector])".to_owned(),
        ),
        (
            "ceilingz",
            "toInt32(sector_ceilingheight[1 + sector])".to_owned(),
        ),
        (
            "z",
            format!(
                "toInt32(if(bitAnd(flags, {MF_SPAWNCEILING}) != 0, ceilingz - height, floorz))"
            ),
        ),
        ("sp_x", spawnpoint("thing_x")),
        ("sp_y", spawnpoint("thing_y")),
        ("sp_angle", spawnpoint("thing_angle")),
        ("sp_type", spawnpoint("thing_type")),
        ("sp_options", spawnpoint("thing_options")),
    ];
    format!(
        "SELECT\n{}\nFROM\n(\n{}\n)\nWHERE kind != 0",
        fields
            .iter()
            .map(|(name, expr)| format!("    {expr} AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        indent(&thing_rows(db))
    )
}

/// The mobj columns of the state row, plus the counts the spawn produced.
fn mobjs(db: &str) -> String {
    let by_slot = |column: &str| ordered("thing", column);
    let mut columns = vec![
        (
            "m_id".to_owned(),
            format!(
                "arrayMap(n -> toUInt32(n), arrayEnumerate({}))",
                by_slot("x")
            ),
        ),
        ("m_linkseq".to_owned(), "m_id".to_owned()),
        ("m_player".to_owned(), by_slot("player")),
        (
            "m_target".to_owned(),
            format!("arrayMap(each -> toUInt32(0), {})", by_slot("thing")),
        ),
        ("m_tracer".to_owned(), "m_target".to_owned()),
        (
            "m_momx".to_owned(),
            format!("arrayMap(each -> toInt32(0), {})", by_slot("thing")),
        ),
        ("m_momy".to_owned(), "m_momx".to_owned()),
        ("m_momz".to_owned(), "m_momx".to_owned()),
        ("m_movedir".to_owned(), "m_momx".to_owned()),
        ("m_movecount".to_owned(), "m_momx".to_owned()),
        ("m_threshold".to_owned(), "m_momx".to_owned()),
        ("m_type".to_owned(), by_slot("type")),
        ("mobj_draws".to_owned(), "toUInt32(sum(draws))".to_owned()),
        ("mobj_count".to_owned(), "toUInt32(count())".to_owned()),
        (
            "totalkills".to_owned(),
            format!("toInt32(countIf(kind = 2 AND bitAnd(flags, {MF_COUNTKILL}) != 0))"),
        ),
        (
            "totalitems".to_owned(),
            format!("toInt32(countIf(kind = 2 AND bitAnd(flags, {MF_COUNTITEM}) != 0))"),
        ),
    ];
    for (column, field) in [
        ("m_x", "x"),
        ("m_y", "y"),
        ("m_z", "z"),
        ("m_angle", "angle"),
        ("m_sprite", "sprite"),
        ("m_frame", "frame"),
        ("m_floorz", "floorz"),
        ("m_ceilingz", "ceilingz"),
        ("m_radius", "radius"),
        ("m_height", "height"),
        ("m_tics", "tics"),
        ("m_state", "state"),
        ("m_flags", "flags"),
        ("m_health", "health"),
        ("m_reactiontime", "reactiontime"),
        ("m_lastlook", "lastlook"),
        ("m_sp_x", "sp_x"),
        ("m_sp_y", "sp_y"),
        ("m_sp_angle", "sp_angle"),
        ("m_sp_type", "sp_type"),
        ("m_sp_options", "sp_options"),
        ("m_subsector", "subsector"),
    ] {
        columns.push((column.to_owned(), by_slot(field)));
    }
    aggregate(&columns, &mobj_rows(db))
}

// ---------------------------------------------------------------------------
// P_SpawnSpecials
// ---------------------------------------------------------------------------

/// One row per sector thinker, in sector order, with the number of the
/// `P_Random` call its spawn makes.
///
/// The light spawns run after every thing has spawned, so their call
/// numbers continue the things' running total.
fn light_rows(db: &str) -> String {
    let flash = sector_kind::LIGHT_FLASH;
    let strobe = sector_kind::STROBE;
    let glow = sector_kind::GLOW;
    let flicker = sector_kind::FIRE_FLICKER;
    // `getNextSector`: the sector on the other side of a two-sided line,
    // and -1 when the line has no other side.
    let neighbour = format!(
        "arrayMap(i -> if(bitAnd(toInt32(line_flags[1 + i]), {ML_TWOSIDED}) = 0, -1, \
         if(line_front[1 + i] = toInt32(s.id), line_back[1 + i], line_front[1 + i])), s.lines)"
    );
    let min_surrounding = format!(
        "toInt32(arrayMin(arrayPushBack(arrayMap(n -> toInt32(sector_lightlevel[1 + n]), \
         arrayFilter(n -> n >= 0, {neighbour})), toInt32(s.lightlevel))))"
    );
    let classified = format!(
        "SELECT\n    \
         s.id AS sector,\n    \
         s.special AS special,\n    \
         toUInt8(multiIf(\
         special = 1, {flash}, \
         special IN (2, 3, 4, 12, 13), {strobe}, \
         special = 8, {glow}, \
         special = 17, {flicker}, \
         0)) AS kind,\n    \
         toUInt32(if(special IN (1, 2, 3, 4), 1, 0)) AS draws,\n    \
         toInt32(s.lightlevel) AS maxlight,\n    \
         {min_surrounding} AS min_surrounding\n\
         FROM {db}.lv_sectors_static AS s\n\
         WHERE kind != 0"
    );
    // The sectors that spawn nothing draw nothing, so leaving them out
    // above does not move anyone's call number.
    let numbered = format!(
        "SELECT\n    \
         *,\n    \
         toUInt32(sum(draws) OVER \
         (ORDER BY sector ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) \
         - draws + (SELECT sum(draws) FROM ({})) + 1) AS first\n\
         FROM\n(\n{}\n)",
        thing_rows(db),
        indent(&classified)
    );
    let draw = "toInt32(rnd[1 + bitAnd(first, 255)])";
    format!(
        "SELECT\n    \
         sector,\n    \
         kind,\n    \
         draws,\n    \
         toInt32(multiIf(\
         kind = {strobe} AND min_surrounding = maxlight, 0, \
         kind = {flicker}, min_surrounding + {FLICKER_LIGHT_MARGIN}, \
         min_surrounding)) AS minlight,\n    \
         maxlight,\n    \
         toInt32(multiIf(\
         kind = {flash}, {FLASH_MINTIME}, \
         kind = {strobe} AND special IN (3, 12), {SLOWDARK}, \
         kind = {strobe}, {FASTDARK}, \
         0)) AS mintime,\n    \
         toInt32(multiIf(\
         kind = {flash}, {FLASH_MAXTIME}, \
         kind = {strobe}, {STROBEBRIGHT}, \
         0)) AS maxtime,\n    \
         toInt32(multiIf(\
         kind = {flash}, bitAnd({draw}, {FLASH_MAXTIME}) + 1, \
         kind = {strobe} AND special IN (12, 13), 1, \
         kind = {strobe}, bitAnd({draw}, 7) + 1, \
         kind = {flicker}, {FLICKER_COUNT}, \
         0)) AS count,\n    \
         toInt32(if(kind = {glow}, -1, 0)) AS direction\n\
         FROM\n(\n{}\n)",
        indent(&numbered)
    )
}

/// The sector-thinker columns of the state row.
fn lights(db: &str) -> String {
    let by_slot = |column: &str| ordered("sector", column);
    let zeros = format!("arrayMap(each -> toInt32(0), {})", by_slot("sector"));
    let mut columns = vec![
        (
            "s_seq".to_owned(),
            format!(
                "arrayMap(n -> toUInt32(n), arrayEnumerate({}))",
                by_slot("sector")
            ),
        ),
        ("s_kind".to_owned(), by_slot("kind")),
        ("s_sector".to_owned(), by_slot("toInt32(sector)")),
        ("s_direction".to_owned(), by_slot("direction")),
        ("s_count".to_owned(), by_slot("count")),
        ("s_minlight".to_owned(), by_slot("minlight")),
        ("s_maxlight".to_owned(), by_slot("maxlight")),
        ("s_mintime".to_owned(), by_slot("mintime")),
        ("s_maxtime".to_owned(), by_slot("maxtime")),
        (
            "s_active".to_owned(),
            format!("arrayMap(each -> toUInt8(1), {})", by_slot("sector")),
        ),
        (
            "s_crush".to_owned(),
            format!("arrayMap(each -> toUInt8(0), {})", by_slot("sector")),
        ),
        ("light_count".to_owned(), "toUInt32(count())".to_owned()),
        ("light_draws".to_owned(), "toUInt32(sum(draws))".to_owned()),
    ];
    // A field none of the light thinkers has is zero.
    for column in [
        "s_type",
        "s_speed",
        "s_dest",
        "s_dest2",
        "s_wait",
        "s_status",
        "s_oldstatus",
        "s_tag",
        "s_texture",
        "s_newspecial",
        "s_activeplat_slot",
        "s_activeceil_slot",
    ] {
        columns.push((column.to_owned(), zeros.clone()));
    }
    aggregate(&columns, &light_rows(db))
}

// ---------------------------------------------------------------------------
// The static tables a tic may change
// ---------------------------------------------------------------------------

/// The sector columns, and what `P_SpawnSpecials` left the specials at.
///
/// A light special is cleared once its thinker exists. A damage or secret
/// special stays, and the sector specials this does not spawn set their
/// bit in `unimplemented`.
fn sectors(db: &str) -> String {
    let by_sector = |column: &str| ordered("id", column);
    let zeros = |cast: &str| format!("arrayMap(each -> {cast}(0), {})", by_sector("id"));
    let columns = vec![
        ("sec_floorheight".to_owned(), by_sector("floorheight")),
        ("sec_ceilingheight".to_owned(), by_sector("ceilingheight")),
        ("sec_floorpic".to_owned(), by_sector("floorpic")),
        ("sec_lightlevel".to_owned(), by_sector("lightlevel")),
        (
            "sec_special".to_owned(),
            by_sector("multiIf(special IN (1, 2, 3, 8, 12, 13, 17), toInt16(0), special)"),
        ),
        ("sec_specialdata".to_owned(), zeros("toUInt32")),
        ("sec_soundtarget".to_owned(), zeros("toUInt32")),
        ("sec_soundtraversed".to_owned(), zeros("toInt32")),
        (
            "totalsecret".to_owned(),
            "toInt32(countIf(special = 9))".to_owned(),
        ),
        (
            "unimplemented".to_owned(),
            format!(
                "toUInt64(if(countIf(special IN (10, 14)) > 0, {}, 0))",
                unimplemented::SECTOR_DOOR
            ),
        ),
    ];
    aggregate(&columns, &format!("SELECT * FROM {db}.lv_sectors_static"))
}

fn lines(db: &str) -> String {
    let columns = vec![("line_special".to_owned(), ordered("id", "special"))];
    aggregate(&columns, &format!("SELECT * FROM {db}.lv_lines"))
}

fn sides(db: &str) -> String {
    let columns = vec![
        ("side_toptexture".to_owned(), ordered("id", "toptexture")),
        ("side_midtexture".to_owned(), ordered("id", "midtexture")),
        (
            "side_bottomtexture".to_owned(),
            ordered("id", "bottomtexture"),
        ),
        (
            "side_textureoffset".to_owned(),
            ordered("id", "textureoffset"),
        ),
    ];
    aggregate(&columns, &format!("SELECT * FROM {db}.lv_sides"))
}

/// Where the row reads its arrays from. Each of these returns one row.
fn sources(db: &str) -> String {
    let sources = [
        ("mobjs", mobjs(db)),
        ("lights", lights(db)),
        ("sec", sectors(db)),
        ("ln", lines(db)),
        ("sd", sides(db)),
    ];
    sources
        .iter()
        .map(|(name, sql)| format!("(\n{}\n) AS {name}", indent(sql)))
        .collect::<Vec<_>>()
        .join(",\n")
}

// ---------------------------------------------------------------------------
// The row
// ---------------------------------------------------------------------------

fn row(db: &str) -> Vec<(&'static str, String)> {
    let mut row = vec![
        ("tic", "toUInt32(0)".to_owned()),
        ("leveltime", "toInt32(0)".to_owned()),
        (
            "prndindex",
            "toUInt8(bitAnd(mobjs.mobj_draws + lights.light_draws, 255))".to_owned(),
        ),
        ("rndindex", "toUInt8(0)".to_owned()),
        (
            "next_seq",
            "toUInt32(mobjs.mobj_count + lights.light_count + 1)".to_owned(),
        ),
        ("next_linkseq", "toUInt32(mobjs.mobj_count + 1)".to_owned()),
        ("paused", "toUInt8(0)".to_owned()),
        ("demo_end", "toUInt8(0)".to_owned()),
        ("totalkills", "mobjs.totalkills".to_owned()),
        ("totalitems", "mobjs.totalitems".to_owned()),
        ("totalsecret", "sec.totalsecret".to_owned()),
        ("unresolved", "toUInt8(0)".to_owned()),
        ("unimplemented", "sec.unimplemented".to_owned()),
        ("dbg_ran", "CAST([], 'Array(UInt32)')".to_owned()),
        ("dbg_prnd", "CAST([], 'Array(UInt8)')".to_owned()),
    ];
    row.extend(pass_through());
    row.extend(anims(db));
    row.extend(buttons());
    row.extend(player());
    row.extend(psprites());
    row.extend(hud());
    row
}

/// The columns a source computed under its own name.
fn pass_through() -> Vec<(&'static str, String)> {
    let mut columns = Vec::new();
    for (source, names) in [
        (
            "mobjs",
            vec![
                "m_id",
                "m_x",
                "m_y",
                "m_z",
                "m_angle",
                "m_sprite",
                "m_frame",
                "m_floorz",
                "m_ceilingz",
                "m_radius",
                "m_height",
                "m_momx",
                "m_momy",
                "m_momz",
                "m_type",
                "m_tics",
                "m_state",
                "m_flags",
                "m_health",
                "m_movedir",
                "m_movecount",
                "m_target",
                "m_reactiontime",
                "m_threshold",
                "m_player",
                "m_lastlook",
                "m_sp_x",
                "m_sp_y",
                "m_sp_angle",
                "m_sp_type",
                "m_sp_options",
                "m_tracer",
                "m_subsector",
                "m_linkseq",
            ],
        ),
        (
            "lights",
            vec![
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
            ],
        ),
        (
            "sec",
            vec![
                "sec_floorheight",
                "sec_ceilingheight",
                "sec_floorpic",
                "sec_lightlevel",
                "sec_special",
                "sec_specialdata",
                "sec_soundtarget",
                "sec_soundtraversed",
            ],
        ),
        ("ln", vec!["line_special"]),
        (
            "sd",
            vec![
                "side_toptexture",
                "side_midtexture",
                "side_bottomtexture",
                "side_textureoffset",
            ],
        ),
    ] {
        for name in names {
            columns.push((name, format!("{source}.{name}")));
        }
    }
    columns
}

/// `R_InitTextures` and `R_InitFlats` number every picture as itself, and
/// `P_InitPicAnims` leaves that alone until a tic animates one.
fn anims(db: &str) -> Vec<(&'static str, String)> {
    let identity = |table: &str| {
        format!("arrayMap(n -> toInt32(n), range((SELECT count() FROM {db}.{table})))")
    };
    vec![
        ("texturetranslation", identity("tex_textures")),
        ("flattranslation", identity("flats")),
    ]
}

/// `P_SpawnSpecials` clears the switch-button table. A button with no line
/// holds -1, which is how the probe writes a null pointer into an array.
fn buttons() -> Vec<(&'static str, String)> {
    vec![
        ("btn_line", constant_array("Int32", &[-1; MAXBUTTONS])),
        ("btn_where", constant_array("UInt8", &[0; MAXBUTTONS])),
        ("btn_texture", constant_array("Int32", &[0; MAXBUTTONS])),
        ("btn_timer", constant_array("Int32", &[0; MAXBUTTONS])),
    ]
}

/// `G_PlayerReborn` then `P_SpawnPlayer`: the player the level starts with.
fn player() -> Vec<(&'static str, String)> {
    vec![
        (
            "p_mo",
            "toUInt32(indexOf(mobjs.m_player, toInt8(0)))".to_owned(),
        ),
        ("p_playerstate", "toUInt8(0)".to_owned()),
        ("p_cmd_forwardmove", "toInt8(0)".to_owned()),
        ("p_cmd_sidemove", "toInt8(0)".to_owned()),
        ("p_cmd_angleturn", "toInt16(0)".to_owned()),
        ("p_cmd_buttons", "toUInt8(0)".to_owned()),
        ("p_viewz", "toInt32(0)".to_owned()),
        ("p_viewheight", format!("toInt32({VIEWHEIGHT})")),
        ("p_deltaviewheight", "toInt32(0)".to_owned()),
        ("p_bob", "toInt32(0)".to_owned()),
        ("p_health", format!("toInt32({INITIAL_HEALTH})")),
        ("p_armorpoints", "toInt32(0)".to_owned()),
        ("p_armortype", "toInt32(0)".to_owned()),
        ("p_powers", constant_array("Int32", &[0; 6])),
        ("p_cards", constant_array("UInt8", &[0; 6])),
        ("p_backpack", "toUInt8(0)".to_owned()),
        ("p_readyweapon", format!("toInt32({WP_PISTOL})")),
        ("p_pendingweapon", format!("toInt32({WP_NOCHANGE})")),
        (
            "p_weaponowned",
            constant_array("Int32", &[1, 1, 0, 0, 0, 0, 0, 0, 0]),
        ),
        (
            "p_ammo",
            constant_array("Int32", &[i64::from(INITIAL_BULLETS), 0, 0, 0]),
        ),
        ("p_maxammo", constant_array("Int32", &MAXAMMO)),
        ("p_attackdown", "toUInt8(1)".to_owned()),
        ("p_usedown", "toUInt8(1)".to_owned()),
        ("p_cheats", "toInt32(0)".to_owned()),
        ("p_refire", "toInt32(0)".to_owned()),
        ("p_killcount", "toInt32(0)".to_owned()),
        ("p_itemcount", "toInt32(0)".to_owned()),
        ("p_secretcount", "toInt32(0)".to_owned()),
        ("p_message", "toUInt64(0)".to_owned()),
        ("p_damagecount", "toInt32(0)".to_owned()),
        ("p_bonuscount", "toInt32(0)".to_owned()),
        ("p_attacker", "toUInt32(0)".to_owned()),
        ("p_extralight", "toInt32(0)".to_owned()),
        ("p_fixedcolormap", "toInt32(0)".to_owned()),
    ]
}

/// `P_SetupPsprites` clears both sprites and `P_BringUpWeapon` puts the
/// ready weapon's up state on the weapon sprite at the bottom of the
/// screen. `P_SetPsprite` runs that state's action on entry, and the guard
/// above says the action is `A_Raise`, which lifts the sprite once and
/// stops short of the top.
fn psprites() -> Vec<(&'static str, String)> {
    vec![
        (
            "psp_state",
            "CAST([up_state, -1], 'Array(Int32)')".to_owned(),
        ),
        (
            "psp_tics",
            "CAST([state_tics[1 + up_state], 0], 'Array(Int32)')".to_owned(),
        ),
        ("psp_sx", constant_array("Int32", &[0, 0])),
        (
            "psp_sy",
            constant_array("Int32", &[WEAPONBOTTOM - RAISESPEED, 0]),
        ),
    ]
}

/// `ST_Start` and `HU_Start`, plus the statics neither of them resets.
fn hud() -> Vec<(&'static str, String)> {
    vec![
        ("st_faceindex", "toInt32(0)".to_owned()),
        ("st_facecount", "toInt32(0)".to_owned()),
        ("st_priority", "toInt32(0)".to_owned()),
        ("st_lastattackdown", format!("toInt32({NO_ATTACK_DOWN})")),
        ("st_oldweaponsowned", "p_weaponowned".to_owned()),
        ("st_oldhealth", format!("toInt32({NO_OLD_HEALTH})")),
        ("st_randomnumber", "toInt32(0)".to_owned()),
        ("st_lastcalc", "toInt32(0)".to_owned()),
        ("st_calc_oldhealth", format!("toInt32({NO_OLD_HEALTH})")),
        ("st_palette", format!("toInt32({NO_PALETTE})")),
        ("st_clock", "toInt32(0)".to_owned()),
        ("hu_message_on", "toUInt8(0)".to_owned()),
        ("hu_message_counter", "toInt32(0)".to_owned()),
        ("hu_message", "toUInt64(0)".to_owned()),
        ("hu_nottobefuckedwith", "toUInt8(0)".to_owned()),
        ("menu_skullanim", format!("toInt32({SKULL_ANIM_COUNTER})")),
        ("menu_whichskull", "toInt32(0)".to_owned()),
        ("turnheld", "toInt32(0)".to_owned()),
    ]
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// One column of `rows` as an array in `key` order.
fn ordered(key: &str, column: &str) -> String {
    format!("arrayMap(t -> t.2, arraySort(t -> t.1, groupArray(({key}, {column}))))")
}

fn aggregate(columns: &[(String, String)], rows: &str) -> String {
    format!(
        "SELECT\n{}\nFROM\n(\n{}\n)",
        columns
            .iter()
            .map(|(name, expr)| format!("    {expr} AS {name}"))
            .collect::<Vec<_>>()
            .join(",\n"),
        indent(rows)
    )
}

fn constant_array(kind: &str, values: &[i64]) -> String {
    format!(
        "CAST([{}], 'Array({kind})')",
        values
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_guards_before_it_spawns() {
        let statements = statements("nat");
        let (guards, row) = statements.split_at(statements.len() - 1);
        assert!(guards.iter().all(|s| s.sql.starts_with("SELECT throwIf")));
        assert!(row[0].sql.starts_with("INSERT INTO nat.native_state"));
        assert!(statements.iter().all(|s| s.body.is_empty()));
    }

    #[test]
    fn the_thing_filter_tests_in_the_engine_s_order() {
        let text = thing_kind("type", "options", "bit");
        let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle}"));
        assert!(at("type = 11, 0") < at("type <= 0, 0"));
        assert!(at("type <= 0, 0") < at("type = 1, 1"));
        assert!(at("type = 1, 1") < at("type <= 4, 0"));
        assert!(at("type <= 4, 0") < at("bitAnd(options, 16)"));
        assert!(at("bitAnd(options, 16)") < at("bitAnd(options, bit)"));
    }

    #[test]
    fn the_row_balances_its_parentheses() {
        for statement in statements("nat") {
            let depth = statement.sql.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{}", statement.summary());
        }
    }
}
