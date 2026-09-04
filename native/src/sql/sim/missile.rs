//! What a monster throws at what it is chasing, from `p_mobj.c`.
//!
//! `P_SpawnMissile` puts the thing in front of the shooter, points it at the
//! target, gives it the speed its type carries, and hands it to
//! `P_CheckMissileSpawn`, which shortens its wait, moves it half a step and
//! sets it off where that step is refused.

use super::map::{self, World, answer};
use super::{maputl, mobj};
use crate::sql::Statement;
use crate::sql::{bind, fixed};

/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `tables.h`
const ANGLETOFINESHIFT: u32 = 19;
const ANGLE_WRAP: i64 = 1 << 32;
/// `p_mobj.c`: how far above the shooter's own feet a missile starts, and
/// what the fuzz a `MF_SHADOW` target draws is shifted by.
const MISSILE_HEIGHT: i64 = 4 * 8 * FRACUNIT;
const FUZZ_SHIFT: u32 = 20;

/// `p_mobj.h`
const MF_SPECIAL: i64 = 1;
const MF_SOLID: i64 = 2;
const MF_SHOOTABLE: i64 = 4;
const MF_NOCLIP: i64 = 0x1000;
const MF_MISSILE: i64 = 0x1_0000;
const MF_SHADOW: i64 = 0x4_0000;

/// Where each field of a missile ask sits in its tuple.
pub mod throwing {
    /// The slot firing it, which becomes the missile's target.
    pub const SOURCE: usize = 1;
    /// The slot it is aimed at.
    pub const DEST: usize = 2;
    /// The `mobjtype` of the missile.
    pub const TYPE: usize = 3;
    /// How many numbers the tic drew before this call's own.
    pub const BASE: usize = 4;
}

/// Where each field of a thrown missile sits in its answer.
///
/// The first eleven are what `P_SpawnMobj` left, in [`mobj::born`]'s order,
/// so a reader of one reads the other. The rest are what `P_SpawnMissile`
/// and `P_CheckMissileSpawn` wrote over them.
pub mod thrown {
    pub const X: usize = 1;
    pub const Y: usize = 2;
    pub const Z: usize = 3;
    pub const TYPE: usize = 4;
    pub const STATE: usize = 5;
    pub const TICS: usize = 6;
    pub const FLOORZ: usize = 7;
    pub const CEILINGZ: usize = 8;
    pub const SUBSECTOR: usize = 9;
    pub const LASTLOOK: usize = 10;
    pub const REACTIONTIME: usize = 11;
    pub const MOMX: usize = 12;
    pub const MOMY: usize = 13;
    pub const MOMZ: usize = 14;
    pub const ANGLE: usize = 15;
    pub const TARGET: usize = 16;
    pub const FLAGS: usize = 17;
    /// 1 where the half-step was refused and the missile went off.
    pub const EXPLODED: usize = 18;
    /// How many numbers the call drew.
    pub const DRAWS: usize = 19;
    /// 1 where the call reached a path this does not write.
    pub const STUCK: usize = 20;
}

/// The ClickHouse type of a [`thrown`] tuple, for a caller that carries a
/// list of them through a fold.
pub const THROWN_TYPE: &str = "Tuple(Int32, Int32, Int32, Int32, Int32, Int32, Int32, Int32, \
                               Int32, Int32, Int32, Int32, Int32, Int32, UInt32, UInt32, Int32, \
                               UInt8, UInt32, UInt8)";

/// The mobj arrays a missile reads, as the tic left them, and where the
/// tic's own random index had got to.
pub struct Throwing<'a> {
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_z: &'a str,
    pub m_radius: &'a str,
    pub m_height: &'a str,
    pub m_flags: &'a str,
    pub prndindex: &'a str,
}

/// What stops the load: a missile type with no speed to divide the
/// distance by, or one that can be stopped and has no frame to go off in.
///
/// `P_SpawnMissile` divides by the speed. `P_ExplodeMissile` enters the
/// death frame rather than removing the thing, and only a missile the map
/// can refuse reaches it.
pub fn guards(db: &str) -> Vec<Statement> {
    let missile = format!("bitAnd(flags, {MF_MISSILE}) != 0");
    [
        (
            format!("{missile} AND speed = 0"),
            "a missile type has no speed to divide the distance by",
        ),
        (
            format!("{missile} AND bitAnd(flags, {MF_NOCLIP}) = 0 AND deathstate = 0"),
            "a missile type the map can stop has no death frame",
        ),
    ]
    .into_iter()
    .map(|(broken, message)| {
        Statement::sql(format!(
            "SELECT throwIf(count() != 0, '{message}')\n     \
             FROM {db}.mobjinfo WHERE {broken}"
        ))
    })
    .collect()
}

/// `P_SpawnMissile` over every ask in `asks`, as a [`thrown`] tuple each.
///
/// Two draws where the step lands: `P_SpawnMobj`'s own for `lastlook` and
/// `P_CheckMissileSpawn`'s for the wait. A `MF_SHADOW` target adds the two
/// the angle fuzz draws, between those; a step the move test refuses adds
/// `P_ExplodeMissile`'s, behind both.
///
/// A missile carries `MF_NOBLOCKMAP` and none of `MF_SOLID`,
/// `MF_SHOOTABLE` or `MF_SPECIAL`, so nothing on the blockmap sees it and
/// it is not in the arrays the move test reads. The move test is asked
/// with the shooter's own slot, which is the thing `PIT_CheckThing` hands
/// back untouched when the missile that reached it came from there.
pub fn spawn(
    asks: &str,
    world: &Throwing<'_>,
    spawning: &mobj::Spawning<'_>,
    map: &World<'_>,
) -> String {
    let (values, body) = thrown(world, spawning, map);
    format!(
        "arrayMap(ms_ask -> {}, {asks})",
        bind::chain_in("msa", &values, &body)
    )
}

/// What one missile works out, as the values a body reads and the
/// [`thrown`] tuple it answers with.
fn thrown(
    world: &Throwing<'_>,
    spawning: &mobj::Spawning<'_>,
    map: &World<'_>,
) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("ms_ask.{field}");
    let from = |array: &str| format!("{array}[ms_source]");
    let dest = |array: &str| format!("{array}[ms_dest]");
    let born = |field: usize| format!("ms_born.{field}");
    let info = |table: &str| format!("{table}[1 + ms_type]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({}) + {nth}, 255)])",
            world.prndindex,
            a(throwing::BASE),
        )
    };
    let across = |axis: &dyn Fn(&str) -> String, array: &str| {
        format!(
            "toInt32(toInt64({}) - toInt64({}))",
            axis(array),
            from(array)
        )
    };

    value("ms_source", format!("toUInt32({})", a(throwing::SOURCE)));
    value("ms_dest", format!("toUInt32({})", a(throwing::DEST)));
    value("ms_type", format!("toInt32({})", a(throwing::TYPE)));
    value("ms_speed", info("mobj_speed"));

    // `P_SpawnMobj` at the shooter's point, four eighths of a unit up.
    value(
        "ms_born",
        format!(
            "{}[1]",
            mobj::spawn_mobj(
                &format!(
                    "[(ms_type, {}, {}, toInt32(toInt64({}) + {MISSILE_HEIGHT}), toUInt32({}))]",
                    from(world.m_x),
                    from(world.m_y),
                    from(world.m_z),
                    a(throwing::BASE),
                ),
                spawning,
            )
        ),
    );

    // The angle at the target, with the fuzz a `MF_SHADOW` one draws. Both
    // draws sit behind the spawn's own.
    value(
        "ms_aimed",
        fixed::point_to_angle(
            &across(&dest, world.m_x),
            &across(&dest, world.m_y),
            "tantoangle",
        ),
    );
    value(
        "ms_fuzzy",
        format!("toUInt8(bitAnd({}, {MF_SHADOW}) != 0)", dest(world.m_flags)),
    );
    value(
        "ms_angle",
        format!(
            "toUInt32(bitAnd(toInt64(ms_aimed) \
             + if(ms_fuzzy = 1, bitShiftLeft({} - {}, {FUZZ_SHIFT}), 0) + {ANGLE_WRAP}, {}))",
            draw("2"),
            draw("3"),
            ANGLE_WRAP - 1,
        ),
    );
    value(
        "ms_fine",
        format!("toUInt32(bitShiftRight(ms_angle, {ANGLETOFINESHIFT}))"),
    );
    value(
        "ms_momx",
        fixed::fixed_mul("ms_speed", &maputl::finecosine("ms_fine")),
    );
    value(
        "ms_momy",
        fixed::fixed_mul("ms_speed", &maputl::finesine("ms_fine")),
    );
    // The height it climbs is the drop divided by the tics the missile
    // takes to cover the distance, held at one tic.
    value(
        "ms_dist",
        format!(
            "greatest(intDiv(toInt64({}), toInt64(ms_speed)), 1)",
            fixed::aprox_distance(&across(&dest, world.m_x), &across(&dest, world.m_y)),
        ),
    );
    value(
        "ms_momz",
        format!(
            "toInt32(intDiv(toInt64({}), ms_dist))",
            across(&dest, world.m_z)
        ),
    );

    // `P_CheckMissileSpawn`: the wait it shortens, then half a step and the
    // move test on where that lands.
    value(
        "ms_short",
        format!(
            "toInt32(greatest({} - bitAnd({}, 3), 1))",
            born(thrown::TICS),
            draw("2 + 2 * toUInt32(ms_fuzzy)"),
        ),
    );
    for (axis, held, mom) in [
        ("x", thrown::X, "ms_momx"),
        ("y", thrown::Y, "ms_momy"),
        ("z", thrown::Z, "ms_momz"),
    ] {
        value(
            &format!("ms_step{axis}"),
            format!("toInt32(toInt64({}) + bitShiftRight({mom}, 1))", born(held)),
        );
    }
    value(
        "ms_move",
        format!(
            "{}[1]",
            map::try_moves(
                &format!(
                    "[{}]",
                    map::asking(
                        "ms_source",
                        "ms_stepx",
                        "ms_stepy",
                        &info("mobj_radius"),
                        &info("mobj_height"),
                        "ms_stepz",
                        &info("mobj_flags"),
                        "0",
                    )
                ),
                map,
            )
        ),
    );
    value("ms_ok", format!("toUInt8(ms_move.{} = 1)", answer::OK));

    // `P_ExplodeMissile`: the momentum goes, the death frame is entered
    // with a wait of its own, another draw shortens it, and the thing stops
    // being a missile.
    value("ms_death", info("mobj_deathstate"));
    value(
        "ms_gone_tics",
        format!(
            "toInt32(greatest(state_tics[1 + ms_death] - bitAnd({}, 3), 1))",
            draw("3 + 2 * toUInt32(ms_fuzzy)"),
        ),
    );

    // What this cannot answer for. `PIT_CheckThing`'s missile branch
    // damages what the step reached before the move test refuses it,
    // `P_TryMove` runs `P_CrossSpecialLine` for a special line the step
    // crossed, and `P_SetMobjState` runs whatever the death frame carries.
    value(
        "ms_touched",
        touching("ms_stepx", "ms_stepy", "ms_stepz", world, map),
    );
    value(
        "ms_stuck",
        format!(
            "toUInt8(ms_touched = 1 OR notEmpty(ms_move.{}) \
             OR (ms_ok = 0 AND state_action[1 + ms_death] != 0))",
            answer::SPECHIT,
        ),
    );

    let landed = |moved: usize, held: usize| {
        format!("toInt32(if(ms_ok = 1, ms_move.{moved}, {}))", born(held))
    };
    let members = [
        "toInt32(ms_stepx)".to_owned(),
        "toInt32(ms_stepy)".to_owned(),
        "toInt32(ms_stepz)".to_owned(),
        format!("toInt32({})", born(thrown::TYPE)),
        format!("toInt32(if(ms_ok = 1, {}, ms_death))", born(thrown::STATE)),
        "toInt32(if(ms_ok = 1, ms_short, ms_gone_tics))".to_owned(),
        landed(answer::FLOORZ, thrown::FLOORZ),
        landed(answer::CEILINGZ, thrown::CEILINGZ),
        landed(answer::SUBSECTOR, thrown::SUBSECTOR),
        format!("toInt32({})", born(thrown::LASTLOOK)),
        format!("toInt32({})", born(thrown::REACTIONTIME)),
        "toInt32(if(ms_ok = 1, ms_momx, 0))".to_owned(),
        "toInt32(if(ms_ok = 1, ms_momy, 0))".to_owned(),
        "toInt32(if(ms_ok = 1, ms_momz, 0))".to_owned(),
        "toUInt32(ms_angle)".to_owned(),
        "toUInt32(ms_source)".to_owned(),
        format!(
            "toInt32(if(ms_ok = 1, {flags}, bitAnd({flags}, {})))",
            !MF_MISSILE,
            flags = info("mobj_flags"),
        ),
        "toUInt8(ms_ok = 0)".to_owned(),
        "toUInt32(2 + 2 * toUInt32(ms_fuzzy) + toUInt32(ms_ok = 0))".to_owned(),
        "toUInt8(ms_stuck)".to_owned(),
    ];
    (values, format!("({})", members.join(", ")))
}

/// Whether any thing but the shooter stands close enough to where the
/// half-step landed for `PIT_CheckThing`'s missile branch to have decided
/// about it, in all three axes.
///
/// The walk covers every thing rather than the blockmap cells around the
/// point. This is a guard, and a wider list can only make it fire sooner.
fn touching(x: &str, y: &str, z: &str, world: &Throwing<'_>, map: &World<'_>) -> String {
    let reach = format!(
        "toInt64({}[k]) + toInt64(mobj_radius[1 + ms_type])",
        world.m_radius
    );
    format!(
        "toUInt8(arrayExists(k -> {alive}[k] = 1 AND k != ms_source \
         AND bitAnd({flags}[k], {}) != 0 \
         AND abs(toInt64({mx}[k]) - toInt64({x})) < {reach} \
         AND abs(toInt64({my}[k]) - toInt64({y})) < {reach} \
         AND toInt64({z}) <= toInt64({mz}[k]) + toInt64({mh}[k]) \
         AND toInt64({z}) + toInt64(mobj_height[1 + ms_type]) >= toInt64({mz}[k]), \
         arrayEnumerate({alive})))",
        MF_SPECIAL | MF_SOLID | MF_SHOOTABLE,
        flags = world.m_flags,
        mx = world.m_x,
        my = world.m_y,
        mz = world.m_z,
        mh = world.m_height,
        alive = map.alive,
    )
}

/// What one state column of a newly thrown missile holds, read out of the
/// tables from its [`thrown`] tuple. `spawn` names one such tuple.
///
/// [`mobj::ASSIGNED_COLUMNS`] answer `None` here as they do for a plain
/// spawn: the identity a thinker takes and the order its sector lists it in
/// are the caller's.
pub fn born_column(column: &str, spawn: &str) -> Option<String> {
    let at = |field: usize| format!("{spawn}.{field}");
    let info = |table: &str| format!("{table}[1 + {}]", at(thrown::TYPE));
    let state = |table: &str| format!("{table}[1 + {}]", at(thrown::STATE));
    if mobj::ASSIGNED_COLUMNS.contains(&column) {
        return None;
    }
    Some(match column {
        "m_x" => at(thrown::X),
        "m_y" => at(thrown::Y),
        "m_z" => at(thrown::Z),
        "m_type" => at(thrown::TYPE),
        "m_state" => at(thrown::STATE),
        "m_tics" => at(thrown::TICS),
        "m_floorz" => at(thrown::FLOORZ),
        "m_ceilingz" => at(thrown::CEILINGZ),
        "m_subsector" => at(thrown::SUBSECTOR),
        "m_lastlook" => at(thrown::LASTLOOK),
        "m_reactiontime" => at(thrown::REACTIONTIME),
        "m_momx" => at(thrown::MOMX),
        "m_momy" => at(thrown::MOMY),
        "m_momz" => at(thrown::MOMZ),
        "m_angle" => at(thrown::ANGLE),
        "m_target" => at(thrown::TARGET),
        "m_flags" => at(thrown::FLAGS),
        "m_sprite" => state("state_sprite"),
        "m_frame" => state("state_frame"),
        "m_radius" => info("mobj_radius"),
        "m_height" => info("mobj_height"),
        "m_health" => info("mobj_spawnhealth"),
        "m_player" => "toInt8(-1)".to_owned(),
        "m_tracer" => "toUInt32(0)".to_owned(),
        "m_sp_x" | "m_sp_y" | "m_sp_angle" | "m_sp_type" | "m_sp_options" => {
            "toInt16(0)".to_owned()
        }
        _ => "toInt32(0)".to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Flight
// ---------------------------------------------------------------------------

/// The thing types `PIT_CheckThing` names by hand when it decides whether
/// a missile hits, as the names `info.h` gives them.
const SPECIES: [&str; 3] = ["MT_KNIGHT", "MT_BRUISER", "MT_PLAYER"];

/// The engine tables a missile in flight reads that no other stage does.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let mut constants = vec![(
        "mobj_damage".to_owned(),
        super::table_column(db, "mobjinfo", "damage"),
    )];
    for name in SPECIES {
        constants.push((
            name.to_lowercase(),
            format!("assumeNotNull((SELECT toInt32(id) FROM {db}.mobjtype WHERE name = '{name}'))"),
        ));
    }
    constants
}

/// Where each field of an explosion ask sits in its tuple.
pub mod stopping {
    /// The missile's slot.
    pub const SLOT: usize = 1;
    /// The line the move test named as the one that brought the ceiling
    /// down, or -1. `P_ZMovement` names none, because the sky test is
    /// `P_XYMovement`'s alone.
    pub const CEILINGLINE: usize = 2;
    /// How many numbers the tic drew before this call's own.
    pub const BASE: usize = 3;
}

/// Where each field of what an explosion leaves sits in its tuple.
///
/// The momentum a caller zeroes is not here: `P_ExplodeMissile` sets all
/// three to nothing whatever else it does.
pub mod stopped {
    pub const STATE: usize = 1;
    pub const TICS: usize = 2;
    pub const FLAGS: usize = 3;
    /// 1 where the sky took the missile off the list rather than setting
    /// it off.
    pub const REMOVED: usize = 4;
    /// How many numbers the call drew.
    pub const DRAWS: usize = 5;
    /// 1 where the call reached a path this does not write.
    pub const STUCK: usize = 6;
}

/// The mobj arrays a missile in flight reads.
pub struct Flying<'a> {
    pub m_z: &'a str,
    pub m_height: &'a str,
    pub m_type: &'a str,
    pub m_state: &'a str,
    pub m_tics: &'a str,
    pub m_flags: &'a str,
    pub m_target: &'a str,
    pub prndindex: &'a str,
}

/// `P_ExplodeMissile` over every ask in `asks`, as a [`stopped`] tuple
/// each, with the sky check `P_XYMovement` makes ahead of it.
///
/// One draw apiece, for the wait the death frame is shortened by. A
/// missile the sky takes draws nothing: `P_RemoveMobj` runs instead.
///
/// The sky check is `P_XYMovement`'s. The line the move test named is the
/// last one that brought the ceiling down, which is what the engine leaves
/// in `ceilingline`, and a missile stopped under a two-sided line whose
/// back sector has a sky ceiling is removed rather than set off.
pub fn explode(asks: &str, world: &Flying<'_>) -> String {
    let (values, body) = exploded(world);
    format!(
        "arrayMap(ex_ask -> {}, {asks})",
        bind::chain_in("exa", &values, &body)
    )
}

/// What one explosion works out, as the values a body reads and the
/// [`stopped`] tuple it answers with.
fn exploded(world: &Flying<'_>) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("ex_ask.{field}");
    let at = |array: &str| format!("{array}[ex_slot]");
    let info = |table: &str| format!("{table}[1 + ex_type]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("ex_slot", format!("toUInt32({})", a(stopping::SLOT)));
    value("ex_line", format!("toInt32({})", a(stopping::CEILINGLINE)));
    value("ex_type", format!("toInt32({})", at(world.m_type)));
    value(
        "ex_sky",
        "toUInt8(ex_line != -1 AND line_side1[1 + ex_line] != -1 \
         AND sec_ceilingpic[1 + line_back[1 + ex_line]] = skyflatnum)"
            .to_owned(),
    );
    value("ex_death", info("mobj_deathstate"));
    value(
        "ex_tics",
        format!(
            "toInt32(greatest(state_tics[1 + ex_death] - bitAnd(toInt64(rnd[1 + bitAnd(\
             toUInt32({}) + toUInt32({}) + 1, 255)]), 3), 1))",
            world.prndindex,
            a(stopping::BASE),
        ),
    );
    // `P_SetMobjState` runs whatever the death frame carries. No missile
    // type the engine ships carries one bar the rocket, whose `A_Explode`
    // is not written here.
    let members = [
        format!("toInt32(if(ex_sky = 1, {}, ex_death))", at(world.m_state)),
        format!("toInt32(if(ex_sky = 1, {}, ex_tics))", at(world.m_tics)),
        format!(
            "toInt32(if(ex_sky = 1, {held}, bitAnd({held}, {})))",
            !MF_MISSILE,
            held = at(world.m_flags),
        ),
        "toUInt8(ex_sky)".to_owned(),
        "toUInt32(if(ex_sky = 1, 0, 1))".to_owned(),
        "toUInt8(ex_sky = 0 AND state_action[1 + ex_death] != 0)".to_owned(),
    ];
    (values, format!("({})", members.join(", ")))
}

/// Where each field of an impact ask sits in its tuple.
pub mod striking {
    /// The missile's slot.
    pub const SLOT: usize = 1;
    /// The slots the move test's box reached, in the order
    /// `P_BlockThingsIterator` reached them.
    pub const TOUCHED: usize = 2;
    /// How many numbers the tic drew before this call's own.
    pub const BASE: usize = 3;
}

/// Where each field of what an impact decides sits in its tuple.
pub mod struck {
    /// The slot the missile stopped on, 0 for none.
    pub const HIT: usize = 1;
    /// 1 where the walk answered no, which refuses the move and sets the
    /// missile off.
    pub const BLOCKED: usize = 2;
    /// What the hit does, 0 where the missile stopped on something it does
    /// not damage.
    pub const DAMAGE: usize = 3;
    /// How many numbers the call drew.
    pub const DRAWS: usize = 4;
}

/// `PIT_CheckThing`'s missile branch over every ask in `asks`, as a
/// [`struck`] tuple each.
///
/// The walk reaches the things in order and the first that decides ends
/// it. A thing the missile passes over or under is skipped, and so is
/// whatever fired it. A thing of the shooter's own species stops the
/// missile without damage unless it is the player. A thing that cannot be
/// shot stops it if it is solid. Anything else takes `(P_Random()%8+1)`
/// times the missile's own damage, which is the call's one draw.
///
/// The missile itself is not in the list: the move test excludes the slot
/// it is asked about, and a missile carries `MF_NOBLOCKMAP` so nothing
/// else's walk reaches it either. A dehacked patch can let a species hurt
/// its own; no patch is loaded, so the rule always holds.
pub fn impact(asks: &str, world: &Flying<'_>) -> String {
    let (values, body) = strikes(world);
    format!(
        "arrayMap(st_ask -> {}, {asks})",
        bind::chain_in("sta", &values, &body)
    )
}

/// What one impact works out, as the values a body reads and the
/// [`struck`] tuple it answers with.
fn strikes(world: &Flying<'_>) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("st_ask.{field}");
    let at = |array: &str| format!("{array}[st_slot]");
    let hit = |array: &str| format!("{array}[k]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("st_slot", format!("toUInt32({})", a(striking::SLOT)));
    value("st_touched", a(striking::TOUCHED));
    value("st_type", format!("toInt32({})", at(world.m_type)));
    value("st_target", format!("toUInt32({})", at(world.m_target)));
    value(
        "st_shot_type",
        format!(
            "toInt32(if(st_target = 0, -1, {}[st_target]))",
            world.m_type
        ),
    );
    // 0 walks on, 1 stops without damage, 2 stops and damages.
    let species = format!(
        "st_target != 0 AND ({t} = st_shot_type \
         OR (st_shot_type = mt_knight AND {t} = mt_bruiser) \
         OR (st_shot_type = mt_bruiser AND {t} = mt_knight))",
        t = hit(world.m_type),
    );
    let decision = format!(
        "toUInt8(multiIf(\
         toInt64({mz}) > toInt64({tz}) + toInt64({th}), 0, \
         toInt64({mz}) + toInt64({mh}) < toInt64({tz}), 0, \
         k = st_target, 0, \
         ({species}) AND {t} != mt_player, 1, \
         bitAnd({tf}, {MF_SHOOTABLE}) = 0, toUInt8(bitAnd({tf}, {MF_SOLID}) != 0), \
         2))",
        mz = at(world.m_z),
        mh = at(world.m_height),
        tz = hit(world.m_z),
        th = hit(world.m_height),
        tf = hit(world.m_flags),
        t = hit(world.m_type),
    );
    value(
        "st_decided",
        format!("arrayMap(k -> {decision}, st_touched)"),
    );
    value(
        "st_at",
        "toUInt32(indexOf(arrayMap(d -> toUInt8(d != 0), st_decided), toUInt8(1)))".to_owned(),
    );
    value(
        "st_hit",
        "toUInt32(if(st_at = 0, 0, st_touched[st_at]))".to_owned(),
    );
    value(
        "st_damages",
        "toUInt8(st_at != 0 AND st_decided[st_at] = 2)".to_owned(),
    );
    value(
        "st_damage",
        format!(
            "toInt32(if(st_damages = 0, 0, (rnd[1 + bitAnd(toUInt32({}) + toUInt32({}) + 1, 255)] \
             % 8 + 1) * mobj_damage[1 + st_type]))",
            world.prndindex,
            a(striking::BASE),
        ),
    );
    let members = [
        "toUInt32(st_hit)".to_owned(),
        "toUInt8(st_at != 0)".to_owned(),
        "toInt32(st_damage)".to_owned(),
        "toUInt32(st_damages)".to_owned(),
    ];
    (values, format!("({})", members.join(", ")))
}

/// The `inter::hurting` ask a hit makes: the thing it reached, the missile
/// as the inflictor, and whatever fired the missile as the source.
///
/// `struck` names one [`struck`] tuple and `slot` the missile. `base` is
/// how many numbers the tic drew before the impact's own, so the damage
/// call's draws sit behind the one the impact made.
pub fn damage_ask(struck: &str, slot: &str, m_target: &str, base: &str) -> String {
    format!(
        "(toUInt32({struck}.{hit}), toUInt32({slot}), toUInt32({m_target}[{slot}]), \
         toInt32({struck}.{damage}), toUInt32({base}) + toUInt32({struck}.{draws}))",
        hit = struck::HIT,
        damage = struck::DAMAGE,
        draws = struck::DRAWS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables;

    fn world() -> Throwing<'static> {
        Throwing {
            m_x: "m_x",
            m_y: "m_y",
            m_z: "m_z",
            m_radius: "m_radius",
            m_height: "m_height",
            m_flags: "m_flags",
            prndindex: "prndindex",
        }
    }

    fn spawning() -> mobj::Spawning<'static> {
        mobj::Spawning {
            floorheight: "floorheight",
            ceilingheight: "ceilingheight",
            prndindex: "prndindex",
            skill: "skill",
        }
    }

    fn map() -> World<'static> {
        World {
            m_x: "m_x",
            m_y: "m_y",
            m_radius: "m_radius",
            m_flags: "m_flags",
            m_linkseq: "m_linkseq",
            alive: "m_alive",
            floorheight: "floorheight",
            ceilingheight: "ceilingheight",
            line_special: "line_special",
        }
    }

    fn sql() -> String {
        spawn("asks", &world(), &spawning(), &map())
    }

    fn named(name: &str) -> String {
        let (values, _) = thrown(&world(), &spawning(), &map());
        values
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, expr)| expr.clone())
            .unwrap_or_else(|| panic!("the call names {name}"))
    }

    /// The draws, in the order the engine makes them: the spawn's own, the
    /// two the fuzz makes, the one that shortens the wait, and the one the
    /// explosion makes.
    #[test]
    fn a_fuzzy_target_moves_every_draw_behind_it() {
        let (_, body) = thrown(&world(), &spawning(), &map());
        assert!(
            body.contains("toUInt32(2 + 2 * toUInt32(ms_fuzzy) + toUInt32(ms_ok = 0))"),
            "{body}"
        );
        assert!(named("ms_short").contains("2 + 2 * toUInt32(ms_fuzzy)"));
        assert!(named("ms_gone_tics").contains("3 + 2 * toUInt32(ms_fuzzy)"));
        assert!(named("ms_angle").contains("+ 2, 255"));
        assert!(named("ms_angle").contains("+ 3, 255"));
    }

    /// The height it climbs is the drop between the two things, not the
    /// drop from where the missile is put.
    #[test]
    fn the_climb_is_measured_between_the_two_things() {
        assert_eq!(
            named("ms_momz"),
            "toInt32(intDiv(toInt64(toInt32(toInt64(m_z[ms_dest]) - toInt64(m_z[ms_source]))), \
             ms_dist))"
        );
    }

    /// The move test and the spawn each appear once however many missiles
    /// the tic throws.
    #[test]
    fn each_primitive_appears_once() {
        let sql = sql();
        assert_eq!(sql.matches("arrayMap(mv ->").count(), 1, "{sql}");
        assert_eq!(sql.matches("arrayMap(sp_ask ->").count(), 1, "{sql}");
        assert_eq!(sql.matches("arrayMap(ms_ask ->").count(), 1, "{sql}");
    }

    /// Every column a thrown missile carries answers in the column's own
    /// type, and the two the caller assigns answer nothing.
    #[test]
    fn every_state_column_is_answered_or_assigned() {
        for column in mobj::ASSIGNED_COLUMNS {
            assert_eq!(born_column(column, "b"), None, "{column}");
        }
        for column in super::super::state_columns() {
            if column.starts_with("m_") && !mobj::ASSIGNED_COLUMNS.contains(&column) {
                assert!(born_column(column, "b").is_some(), "{column}");
            }
        }
        assert_eq!(born_column("m_player", "b").as_deref(), Some("toInt8(-1)"));
        assert_eq!(born_column("m_sp_x", "b").as_deref(), Some("toInt16(0)"));
    }

    /// The tuple's type lists one member per field the answer names.
    #[test]
    fn the_tuple_type_has_one_member_per_field() {
        assert_eq!(THROWN_TYPE.matches(',').count() + 1, thrown::STUCK);
    }

    /// Every type the engine ships as a missile has a speed to divide by,
    /// and every one the map can stop has a frame to go off in, which is
    /// what the load guards check on the tables as they stand. The boss
    /// cube is the one that carries `MF_NOCLIP` and no death frame.
    #[test]
    fn every_missile_type_has_a_speed_and_can_go_off_where_it_is_stopped() {
        let info = tables::table("mobjinfo").unwrap();
        let flags = info.ints("flags").unwrap();
        let speed = info.ints("speed").unwrap();
        let deathstate = info.ints("deathstate").unwrap();
        let (mut missiles, mut noclip) = (0, 0);
        for (at, held) in flags.iter().enumerate() {
            if held & MF_MISSILE == 0 {
                continue;
            }
            missiles += 1;
            assert!(speed[at] != 0, "type {at} has no speed");
            if held & MF_NOCLIP != 0 {
                noclip += 1;
                continue;
            }
            assert!(deathstate[at] != 0, "type {at} has no death frame");
        }
        assert!(missiles > 5, "the table carries missiles: {missiles}");
        assert_eq!(noclip, 1, "one missile type is not stopped by the map");
    }

    fn flying() -> Flying<'static> {
        Flying {
            m_z: "m_z",
            m_height: "m_height",
            m_type: "m_type",
            m_state: "m_state",
            m_tics: "m_tics",
            m_flags: "m_flags",
            m_target: "m_target",
            prndindex: "prndindex",
        }
    }

    /// The sky check reads the back sector of the line the move test
    /// named, and a missile the sky takes draws nothing.
    #[test]
    fn a_sky_ceiling_removes_the_missile_and_draws_nothing() {
        let (values, body) = exploded(&flying());
        let sky = values.iter().find(|(name, _)| name == "ex_sky").unwrap();
        assert!(
            sky.1
                .contains("sec_ceilingpic[1 + line_back[1 + ex_line]] = skyflatnum"),
            "{sky:?}"
        );
        assert!(body.contains("toUInt32(if(ex_sky = 1, 0, 1))"), "{body}");
    }

    /// The explosion takes `MF_MISSILE` off, and leaves the flags alone
    /// where the sky removed the thing instead.
    #[test]
    fn the_explosion_stops_the_thing_being_a_missile() {
        let (_, body) = exploded(&flying());
        assert!(
            body.contains(&format!("bitAnd(m_flags[ex_slot], {})", !MF_MISSILE)),
            "{body}"
        );
    }

    /// The impact draws once, and only where it damages what it reached.
    #[test]
    fn an_impact_draws_only_where_it_damages() {
        let (values, body) = strikes(&flying());
        assert!(body.contains("toUInt32(st_damages)"), "{body}");
        let damage = values.iter().find(|(name, _)| name == "st_damage").unwrap();
        assert!(
            damage.1.contains("% 8 + 1) * mobj_damage[1 + st_type]"),
            "{damage:?}"
        );
    }

    /// The damage ask names the missile as the inflictor and whatever
    /// fired it as the source, in `inter::hurting`'s order.
    #[test]
    fn the_damage_ask_puts_the_missile_in_the_inflictor_slot() {
        let ask = damage_ask("st", "k", "m_target", "base");
        assert_eq!(
            ask,
            "(toUInt32(st.1), toUInt32(k), toUInt32(m_target[k]), toInt32(st.3), \
             toUInt32(base) + toUInt32(st.4))"
        );
        assert_eq!(struck::HIT, 1);
        assert_eq!(super::super::inter::hurting::INFLICTOR, 2);
        assert_eq!(super::super::inter::hurting::SOURCE, 3);
    }

    /// The types `PIT_CheckThing` names by hand come from `mobjtype`
    /// inside the statement rather than out of the generator.
    #[test]
    fn every_type_the_impact_names_comes_from_the_table() {
        let names: Vec<String> = constants("nat")
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("mt_"))
            .collect();
        assert_eq!(names.len(), SPECIES.len());
        let sql = impact("asks", &flying());
        for name in names {
            assert!(sql.contains(&name), "{name}");
        }
    }

    #[test]
    fn the_flight_expressions_balance_their_parentheses() {
        for sql in [explode("asks", &flying()), impact("asks", &flying())] {
            let depth = sql.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{sql}");
        }
    }

    #[test]
    fn the_expression_balances_its_parentheses() {
        let depth = sql().chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
    }
}
