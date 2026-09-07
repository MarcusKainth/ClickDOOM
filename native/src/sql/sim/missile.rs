//! What a monster throws at what it is chasing, from `p_mobj.c`.
//!
//! `P_SpawnMissile` puts the thing in front of the shooter, points it at the
//! target, gives it the speed its type carries, and hands it to
//! `P_CheckMissileSpawn`, which shortens its wait, moves it half a step and
//! sets it off where that step is refused.

use super::map::{self, World, answer};
use super::{inter, maputl, mobj};
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
const MF_NOGRAVITY: i64 = 0x200;
const MF_FLOAT: i64 = 0x4000;
const MF_DROPOFF: i64 = 0x400;
const MF_SKULLFLY: i64 = 0x100_0000;
/// `p_mobj.c`: the fastest one step carries, when the move is split.
const MAXMOVE: i64 = 30 << 16;
/// `p_map.c`: how far a thing steps up, and how far it stands over a
/// dropoff.
const MAXSTEP: i64 = 24 << 16;
/// `p_spec.c`: `P_CrossSpecialLine` returns at once for a non-player thing
/// of one of these types, so a special line the tic's own thrown missile
/// crosses is a no-op for it and unresolved for anything else.
const NO_SPECIAL: [&str; 6] = [
    "MT_ROCKET",
    "MT_PLASMA",
    "MT_BFG",
    "MT_TROOPSHOT",
    "MT_HEADSHOT",
    "MT_BRUISERSHOT",
];

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

/// [`spawn`] over an ask list, folded rather than mapped, as an array of
/// [`thrown`] tuples.
///
/// A map runs every function in its body once even on an empty list, and
/// this body is `P_SpawnMissile` and the move test under it. A fold runs
/// its body only where the list has an element, so a tic that throws
/// nothing pays for the fold and nothing under it.
pub fn spawn_fold(
    asks: &str,
    world: &Throwing<'_>,
    spawning: &mobj::Spawning<'_>,
    map: &World<'_>,
) -> String {
    let (values, body) = thrown(world, spawning, map);
    format!(
        "arrayFold((ms_held, ms_ask) -> arrayPushBack(ms_held, {}), {asks}, \
         CAST([] AS Array({THROWN_TYPE})))",
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
///
/// `MT_TROOPSHOT` is not here: `mobj::constants` already binds it, and a
/// name bound twice in one `WITH` list is one whose value depends on which
/// binding the server keeps.
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
    for name in NO_SPECIAL {
        if name == "MT_TROOPSHOT" {
            continue;
        }
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
    pub m_momx: &'a str,
    pub m_momy: &'a str,
    pub m_momz: &'a str,
    pub m_floorz: &'a str,
    pub m_ceilingz: &'a str,
    pub m_subsector: &'a str,
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

/// A call nobody made, for a caller that folds over a list that carries at
/// most one.
pub fn no_stopped() -> String {
    "(toInt32(0), toInt32(0), toInt32(0), toUInt8(0), toUInt32(0), toUInt8(0))".to_owned()
}

/// [`explode`] over an ask list that carries at most one, folded rather
/// than mapped.
///
/// A map runs every function in its body once even on an empty list, and
/// this body is the whole routine. A fold runs its body only where the list
/// has an element, so a caller with nothing to explode pays for the fold
/// and nothing under it. The answer is the last ask in the list, and
/// [`no_stopped`] is what an empty one gives.
pub fn explode_fold(asks: &str, world: &Flying<'_>) -> String {
    let (values, body) = exploded(world);
    format!(
        "arrayFold((ex_held, ex_ask) -> {}, {asks}, {})",
        bind::chain_in("exa", &values, &body),
        no_stopped(),
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

/// A call nobody made, for a caller that folds over a list that carries at
/// most one.
pub fn no_struck() -> String {
    "(toUInt32(0), toUInt8(0), toInt32(0), toUInt32(0))".to_owned()
}

/// [`impact`] over an ask list that carries at most one, folded rather than
/// mapped.
///
/// A map runs every function in its body once even on an empty list, and
/// this body is the whole routine. A fold runs its body only where the list
/// has an element, so a caller with nothing to hit pays for the fold and
/// nothing under it. The answer is the last ask in the list, and
/// [`no_struck`] is what an empty one gives.
pub fn impact_fold(asks: &str, world: &Flying<'_>) -> String {
    let (values, body) = strikes(world);
    format!(
        "arrayFold((st_held, st_ask) -> {}, {asks}, {})",
        bind::chain_in("sta", &values, &body),
        no_struck(),
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

// ---------------------------------------------------------------------------
// P_MobjThinker
// ---------------------------------------------------------------------------

/// Where each field of a `thinks` ask sits in its tuple.
pub mod thinking {
    /// The slot to run `P_MobjThinker` for.
    pub const SLOT: usize = 1;
    /// How many numbers the tic drew before this call's own.
    pub const BASE: usize = 2;
}

/// Where each field of a `thinks` answer sits in its tuple.
pub mod thought {
    pub const X: usize = 1;
    pub const Y: usize = 2;
    pub const Z: usize = 3;
    pub const FLOORZ: usize = 4;
    pub const CEILINGZ: usize = 5;
    pub const SUBSECTOR: usize = 6;
    pub const MOMX: usize = 7;
    pub const MOMY: usize = 8;
    pub const MOMZ: usize = 9;
    pub const STATE: usize = 10;
    pub const TICS: usize = 11;
    pub const FLAGS: usize = 12;
    /// The slot `PIT_CheckThing`'s missile branch damaged, 0 for none.
    pub const HURT_TARGET: usize = 13;
    /// What `P_DamageMobj` leaves the slot above with, read where it is
    /// not 0.
    pub const HURT: usize = 14;
    /// How many numbers the call drew.
    pub const DRAWS: usize = 15;
    /// 1 where the call reached a path this does not write.
    pub const STUCK: usize = 16;
}

/// `P_XYMovement` clamps each axis to `MAXMOVE` before it starts.
fn clamp(mom: &str) -> String {
    format!("toInt32(least(greatest(toInt64({mom}), -{MAXMOVE}), {MAXMOVE}))")
}

/// The ClickHouse type of a [`thought`] tuple, for a caller that carries a
/// list of them through a fold. Its own [`thought::HURT`] member is
/// [`inter::HURT_TYPE`], named once rather than spelled twice so the two
/// cannot drift apart.
pub fn thought_type() -> String {
    format!(
        "Tuple(Int32, Int32, Int32, Int32, Int32, Int32, Int32, Int32, Int32, Int32, Int32, \
         Int32, UInt32, {}, UInt32, UInt8)",
        inter::HURT_TYPE
    )
}

/// The mobj arrays a missile already on the list reads for its own move,
/// its own fall and its own state cycle, plus what a thing it damages
/// reads. `slot` names which mobj in these `flying` and `hurting` reads
/// the missile itself, over the same arrays a caller driving anything
/// else's move already holds.
///
/// The player's own health, armour, damagecount and attacker thread
/// through every missile the list carries, in order: each one's own
/// `P_DamageMobj` reads what the missile before it left, the way
/// [`inter::damage_fold`] threads them for a caller with only one ask.
/// `start` is the tic's own player fields for the first list this runs
/// over, or the previous list's own final fields for one chained after
/// it; the fold's own final tuple carries them on for a caller to chain
/// further still.
pub fn thinks_fold(
    asks: &str,
    start: &str,
    map: &World<'_>,
    flying: &Flying<'_>,
    hurting: &inter::Hurting<'_>,
) -> String {
    let (values, body) = thought_of(map, flying, hurting);
    let step = bind::chain_in("tka", &values, &body);
    let folded = bind::chain_in(
        "tkb",
        &[("tk_result".to_owned(), step)],
        "(arrayPushBack(tk_held.1, tk_result.1), tk_result.2, \
         arrayConcat(tk_held.3, tk_result.3))",
    );
    format!(
        "arrayFold((tk_held, tk_ask) -> {folded}, {asks}, \
         (CAST([] AS Array({})), {start}, CAST([] AS Array({}))))",
        thought_type(),
        mobj::SPAWN_ASK_TYPE,
    )
}

/// `p_inter.c`: how far below the thing that hit it a target has to stand
/// to be knocked over, and the most damage that can do it.
const FALL_HEIGHT: i64 = 64 * FRACUNIT;
const FALL_DAMAGE: i64 = 40;

/// How many numbers [`thinks_fold`] draws for one ask in `asks`, worked
/// out before any of them runs.
///
/// Whether the missile's move lands and whether it damages something is
/// decided by geometry alone, so `impact_fold`'s own answer for it does
/// not depend on where in the tic's draws this call starts; only the
/// damage itself does, so this reads the missile's own worst case
/// instead, the same tic-start-only reading a claw's own count already
/// makes of its target's health and flags.
pub fn draws(
    asks: &str,
    map: &World<'_>,
    flying: &Flying<'_>,
    hurting: &inter::Hurting<'_>,
) -> String {
    let (values, body) = missile_draws(map, flying, hurting);
    format!(
        "arrayMap(mkd_ask -> {}, {asks})",
        bind::chain_in("mkd", &values, &body)
    )
}

/// Whether [`draws`]'s worst-case reading of one ask in `asks` could
/// undercount it: the move lands on something, its target's health sits
/// under the missile's own worst-case damage, and the height between
/// them clears the fall check.
///
/// `P_DamageMobj` draws the extra fall-over number only where a real
/// roll under the worst case still exceeds the target's health, and that
/// roll is not known until it runs.
pub fn unsure(
    asks: &str,
    map: &World<'_>,
    flying: &Flying<'_>,
    hurting: &inter::Hurting<'_>,
) -> String {
    format!(
        "arrayMap(mkd_ask -> {}, {asks})",
        bind::chain_in(
            "mkd",
            &missile_shape(map, flying, hurting),
            "toUInt8(mkd_unsure)"
        )
    )
}

/// What one ask's shape decides before any random number runs: whether
/// the move lands, whether it reaches something, and whether a real
/// damage roll under [`draws`]'s worst-case reading could still knock
/// its target down.
fn missile_shape(
    map: &World<'_>,
    flying: &Flying<'_>,
    hurting: &inter::Hurting<'_>,
) -> Vec<(String, String)> {
    let a = |field: usize| format!("mkd_ask.{field}");
    let at = |array: &str| format!("{array}[mkd_slot]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("mkd_slot", format!("toUInt32({})", a(thinking::SLOT)));
    value("mkd_type", format!("toInt32({})", at(flying.m_type)));
    value("mkd_radius", format!("toInt64({})", at(map.m_radius)));
    value("mkd_height", format!("toInt64({})", at(flying.m_height)));
    value("mkd_x0", format!("toInt64({})", at(map.m_x)));
    value("mkd_y0", format!("toInt64({})", at(map.m_y)));
    value("mkd_z0", format!("toInt64({})", at(flying.m_z)));
    value("mkd_flags0", format!("toInt32({})", at(flying.m_flags)));
    value("mkd_momx0", format!("toInt64({})", at(flying.m_momx)));
    value("mkd_momy0", format!("toInt64({})", at(flying.m_momy)));
    value(
        "mkd_moving",
        "toUInt8(mkd_momx0 != 0 OR mkd_momy0 != 0)".to_owned(),
    );
    value("mkd_cx", clamp("mkd_momx0"));
    value("mkd_cy", clamp("mkd_momy0"));
    value(
        "mkd_split",
        format!(
            "toUInt8(mkd_cx > {half} OR mkd_cy > {half})",
            half = MAXMOVE / 2
        ),
    );
    value(
        "mkd_ran",
        "toUInt8(mkd_moving = 1 AND mkd_split = 0)".to_owned(),
    );
    value("mkd_ptryx", "mkd_x0 + toInt64(mkd_cx)".to_owned());
    value("mkd_ptryy", "mkd_y0 + toInt64(mkd_cy)".to_owned());
    value(
        "mkd_xy_asks",
        format!(
            "if(mkd_ran = 1, [{}], CAST([] AS Array(Tuple(UInt32, Int32, Int32, Int32, Int32, \
             Int32, Int32, UInt8))))",
            map::asking(
                "mkd_slot",
                "mkd_ptryx",
                "mkd_ptryy",
                "mkd_radius",
                "mkd_height",
                "mkd_z0",
                "toInt32(mkd_flags0)",
                "0",
            )
        ),
    );
    value(
        "mkd_xy_try",
        format!("{}[1]", map::try_moves("mkd_xy_asks", map)),
    );
    value(
        "mkd_xy_touch_asks",
        format!(
            "if(mkd_ran = 1, [(mkd_slot, mkd_xy_try.{touched}, toUInt32(0))], \
             CAST([] AS Array(Tuple(UInt32, Array(UInt32), UInt32))))",
            touched = answer::TOUCHED,
        ),
    );
    value("mkd_hit", impact_fold("mkd_xy_touch_asks", flying));
    value(
        "mkd_worst",
        "toInt32(8 * mobj_damage[1 + mkd_type])".to_owned(),
    );
    let on = |array: &str| format!("{array}[mkd_hit.{}]", struck::HIT);
    value(
        "mkd_unsure",
        format!(
            "toUInt8(mkd_hit.{draws} != 0 AND toInt32({health}) < {FALL_DAMAGE} \
             AND toInt64({tz}) - mkd_z0 > {FALL_HEIGHT})",
            draws = struck::DRAWS,
            health = on(hurting.m_health),
            tz = on(hurting.m_z),
        ),
    );
    values
}

fn missile_draws(
    map: &World<'_>,
    flying: &Flying<'_>,
    hurting: &inter::Hurting<'_>,
) -> (Vec<(String, String)>, String) {
    let mut values = missile_shape(map, flying, hurting);
    let source = format!("{}[mkd_slot]", hurting.m_target);
    values.push((
        "mkd_hurt_asks".to_owned(),
        format!(
            "if(mkd_hit.{draws} != 0, [(mkd_hit.{hit}, mkd_slot, toUInt32({source}), \
             mkd_worst, toUInt32(0))], CAST([] AS Array(Tuple(UInt32, UInt32, UInt32, Int32, \
             UInt32))))",
            draws = struck::DRAWS,
            hit = struck::HIT,
        ),
    ));
    values.push((
        "mkd_hurt_draws".to_owned(),
        format!("arraySum({})", inter::draws("mkd_hurt_asks", hurting)),
    ));
    let body = format!(
        "toUInt32(mkd_hit.{draws} + mkd_hurt_draws)",
        draws = struck::DRAWS,
    );
    (values, body)
}

/// The line `P_ExplodeMissile`'s sky check reads for a blocked move.
///
/// `P_CheckPosition` resets `ceilingline` to none at its own start, walks
/// things before lines, and returns the moment one blocks, so the line
/// walk that sets `ceilingline` never runs when a thing is what stopped
/// the move: a missile a thing blocked always reaches the sky check with
/// none. `blocked` names the impact's own `struck::BLOCKED` field and
/// `try_answer` the move test's own answer tuple.
fn move_ceilingline(blocked: &str, try_answer: &str) -> String {
    format!(
        "toInt32(if({blocked} = 1, -1, {try_answer}.{}))",
        answer::CEILINGLINE
    )
}

/// What one thing's `P_MobjThinker` works out, as the values a body reads
/// and the [`thought`] tuple it answers with.
///
/// `P_XYMovement` moves the thing where its momentum carries it and, on a
/// blocked move, sets it off unless a sky hack takes it instead.
/// `P_ZMovement` clips the height the same way, run whether or not the XY
/// step already exploded it, because `P_MobjThinker` runs it whenever the
/// thing stands off its floor or carries momentum on it, and momentum a
/// blocked XY step zeroed still leaves `z != floorz` where the throw put it
/// above ground. The state cycle runs last and unconditionally, so a
/// missile the XY step just set off has its fresh death-frame tics
/// decremented again the same tic.
fn thought_of(
    map: &World<'_>,
    flying: &Flying<'_>,
    hurting: &inter::Hurting<'_>,
) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("tk_ask.{field}");
    let at = |array: &str| format!("{array}[mn_slot]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("mn_slot", format!("toUInt32({})", a(thinking::SLOT)));
    value("mn_base", format!("toUInt32({})", a(thinking::BASE)));
    value("mn_type", format!("toInt32({})", at(flying.m_type)));
    // `P_TryMove` and `P_ZMovement` read `thing->radius` and
    // `thing->height`, the mobj's own columns, not the type's info-table
    // entry: the two agree for a missile today, and a mover the thinker
    // runs for later may not keep them in step.
    value("mn_radius", format!("toInt64({})", at(map.m_radius)));
    value("mn_height", format!("toInt64({})", at(flying.m_height)));
    value("mn_x0", format!("toInt64({})", at(map.m_x)));
    value("mn_y0", format!("toInt64({})", at(map.m_y)));
    value("mn_z0", format!("toInt64({})", at(flying.m_z)));
    value("mn_flags0", format!("toInt32({})", at(flying.m_flags)));
    value("mn_momx0", format!("toInt64({})", at(flying.m_momx)));
    value("mn_momy0", format!("toInt64({})", at(flying.m_momy)));
    value("mn_momz0", format!("toInt64({})", at(flying.m_momz)));
    value("mn_state0", format!("toInt32({})", at(flying.m_state)));
    value("mn_tics0", format!("toInt32({})", at(flying.m_tics)));
    value(
        "mn_subsector0",
        format!("toInt32({})", at(flying.m_subsector)),
    );
    value("mn_floorz0", format!("toInt64({})", at(flying.m_floorz)));
    value(
        "mn_ceilingz0",
        format!("toInt64({})", at(flying.m_ceilingz)),
    );

    // A thing this does not model: floating towards its target, a charging
    // skull, or one gravity pulls on. Every missile type the tree ships
    // carries `MF_NOGRAVITY`, so `P_ZMovement`'s gravity branch is not
    // written here.
    value(
        "mn_guard",
        format!(
            "toUInt8(bitAnd(mn_flags0, {}) != 0 OR bitAnd(mn_flags0, {}) = 0)",
            MF_FLOAT | MF_SKULLFLY,
            MF_NOGRAVITY,
        ),
    );

    // P_XYMovement. MF_SKULLFLY is guarded above, so the early return is
    // just nothing to spend.
    value(
        "mn_moving",
        "toUInt8(mn_momx0 != 0 OR mn_momy0 != 0)".to_owned(),
    );
    value("mn_cx", clamp("mn_momx0"));
    value("mn_cy", clamp("mn_momy0"));
    // The engine spends a clamped move in at most two parts when either
    // axis is over half of MAXMOVE, and a blocked first part can still
    // spend the second against the corpse it just turned the mobj into.
    // A fresh missile's own first move is refused rather than walked to
    // that second call.
    value(
        "mn_split",
        format!(
            "toUInt8(mn_cx > {half} OR mn_cy > {half})",
            half = MAXMOVE / 2
        ),
    );
    value(
        "mn_ran",
        "toUInt8(mn_moving = 1 AND mn_split = 0)".to_owned(),
    );
    value("mn_ptryx", "mn_x0 + toInt64(mn_cx)".to_owned());
    value("mn_ptryy", "mn_y0 + toInt64(mn_cy)".to_owned());
    value(
        "mn_xy_asks",
        format!(
            "if(mn_ran = 1, [{}], CAST([] AS Array(Tuple(UInt32, Int32, Int32, Int32, Int32, \
             Int32, Int32, UInt8))))",
            map::asking(
                "mn_slot",
                "mn_ptryx",
                "mn_ptryy",
                "mn_radius",
                "mn_height",
                "mn_z0",
                "toInt32(mn_flags0)",
                "0",
            )
        ),
    );
    value(
        "mn_xy_try",
        format!("{}[1]", map::try_moves("mn_xy_asks", map)),
    );
    // P_CheckPosition's own geometry test, recomputed from the primitive's
    // floor, ceiling and dropoff: a missile's blocking decision is not the
    // primitive's generic solid test, so `answer::OK` is not read here.
    value(
        "mn_fits",
        format!(
            "toUInt8(mn_ran = 1 \
             AND toInt64(mn_xy_try.{ceil}) - toInt64(mn_xy_try.{floor}) >= mn_height \
             AND toInt64(mn_xy_try.{ceil}) - mn_z0 >= mn_height \
             AND toInt64(mn_xy_try.{floor}) - mn_z0 <= {maxstep} \
             AND (bitAnd(mn_flags0, {dropoff}) != 0 \
             OR toInt64(mn_xy_try.{floor}) - toInt64(mn_xy_try.{dropoffz}) <= {maxstep}))",
            ceil = answer::CEILINGZ,
            floor = answer::FLOORZ,
            dropoffz = answer::DROPOFFZ,
            dropoff = MF_DROPOFF,
            maxstep = MAXSTEP,
        ),
    );
    value(
        "mn_xy_touch_asks",
        format!(
            "if(mn_ran = 1, [(mn_slot, mn_xy_try.{touched}, mn_base)], \
             CAST([] AS Array(Tuple(UInt32, Array(UInt32), UInt32))))",
            touched = answer::TOUCHED,
        ),
    );
    value("mn_hit", impact_fold("mn_xy_touch_asks", flying));
    value(
        "mn_damage_asks",
        format!(
            "if(mn_hit.{damage} != 0, [{}], \
             CAST([] AS Array(Tuple(UInt32, UInt32, UInt32, Int32, UInt32))))",
            damage_ask("mn_hit", "mn_slot", hurting.m_target, "mn_base"),
            damage = struck::DAMAGE,
        ),
    );
    // `tk_held.2` is what the missile before this one in the same list
    // left the player's own fields at, or the tic's own row where this is
    // the first.
    value(
        "mn_hurt",
        inter::damage_fold("mn_damage_asks", "tk_held.2", hurting),
    );
    value(
        "mn_hurt_target",
        format!(
            "toUInt32(if(mn_hit.{damage} != 0, mn_hit.{hit}, 0))",
            damage = struck::DAMAGE,
            hit = struck::HIT,
        ),
    );
    // `P_KillMobj`'s own drop, for a target this missile's own impact
    // killed. The damage call's own base sits behind the impact's own
    // draws, and `mn_hurt`'s own count already reserves its last draw for
    // the drop, so the spawn's own base is one short of what the call as a
    // whole drew.
    value(
        "mn_drop_asks",
        format!(
            "if(mn_hurt_target != 0 AND mn_hurt.{drop} != -1, \
             [(mn_hurt.{drop}, toInt32({mx}[greatest(mn_hurt_target, 1)]), \
             toInt32({my}[greatest(mn_hurt_target, 1)]), toInt32({onfloorz}), \
             toUInt32(mn_base + mn_hit.{hit_draws} + mn_hurt.{hurt_draws} - 1))], [])",
            drop = inter::hurt::DROP,
            mx = map.m_x,
            my = map.m_y,
            onfloorz = mobj::ONFLOORZ,
            hit_draws = struck::DRAWS,
            hurt_draws = inter::hurt::DRAWS,
        ),
    );
    value(
        "mn_xy_blocked",
        format!(
            "toUInt8(mn_ran = 1 AND (mn_fits = 0 OR mn_xy_try.{line} = 1 OR mn_hit.{blocked} = 1))",
            line = answer::LINE_BLOCKED,
            blocked = struck::BLOCKED,
        ),
    );
    // A special line the move crosses is `P_CrossSpecialLine`'s to run,
    // which is a no-op for the types in `NO_SPECIAL` and unresolved for
    // anything else this throws. Crossing is read from `SPECHIT`
    // membership rather than the side test `P_TryMove` itself uses, which
    // over-refuses for a type outside `NO_SPECIAL`; the side test is
    // `P_CrossSpecialLine`'s own PR to write.
    value(
        "mn_crossed",
        format!(
            "toUInt8(mn_ran = 1 AND mn_xy_blocked = 0 AND notEmpty(mn_xy_try.{spechit}))",
            spechit = answer::SPECHIT,
        ),
    );
    value(
        "mn_crossed_ok",
        format!(
            "toUInt8({})",
            NO_SPECIAL
                .iter()
                .map(|name| format!("mn_type = {}", name.to_lowercase()))
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
    );
    value(
        "mn_xy_ceilingline",
        move_ceilingline(&format!("mn_hit.{}", struck::BLOCKED), "mn_xy_try"),
    );
    value(
        "mn_xy_explode_asks",
        format!(
            "if(mn_xy_blocked = 1, [(mn_slot, mn_xy_ceilingline, \
             mn_base + mn_hit.{hit_draws} + mn_hurt.{hurt_draws})], \
             CAST([] AS Array(Tuple(UInt32, Int32, UInt32))))",
            hit_draws = struck::DRAWS,
            hurt_draws = inter::hurt::DRAWS,
        ),
    );
    value("mn_xy_exploded", explode_fold("mn_xy_explode_asks", flying));
    value(
        "mn_after_xy_removed",
        format!(
            "toUInt8(mn_xy_blocked = 1 AND mn_xy_exploded.{removed} = 1)",
            removed = stopped::REMOVED,
        ),
    );
    value(
        "mn_landed",
        "toUInt8(mn_ran = 1 AND mn_xy_blocked = 0)".to_owned(),
    );
    value(
        "mn_after_xy_x",
        "if(mn_landed = 1, mn_ptryx, mn_x0)".to_owned(),
    );
    value(
        "mn_after_xy_y",
        "if(mn_landed = 1, mn_ptryy, mn_y0)".to_owned(),
    );
    value(
        "mn_after_xy_floorz",
        format!(
            "if(mn_landed = 1, toInt64(mn_xy_try.{floorz}), mn_floorz0)",
            floorz = answer::FLOORZ,
        ),
    );
    value(
        "mn_after_xy_ceilingz",
        format!(
            "if(mn_landed = 1, toInt64(mn_xy_try.{ceilingz}), mn_ceilingz0)",
            ceilingz = answer::CEILINGZ,
        ),
    );
    value(
        "mn_after_xy_subsector",
        format!(
            "if(mn_landed = 1, toInt32(mn_xy_try.{subsector}), mn_subsector0)",
            subsector = answer::SUBSECTOR,
        ),
    );
    value(
        "mn_after_xy_momx",
        "if(mn_xy_blocked = 1, toInt64(0), mn_momx0)".to_owned(),
    );
    value(
        "mn_after_xy_momy",
        "if(mn_xy_blocked = 1, toInt64(0), mn_momy0)".to_owned(),
    );
    value(
        "mn_after_xy_momz",
        "if(mn_xy_blocked = 1, toInt64(0), mn_momz0)".to_owned(),
    );
    value(
        "mn_after_xy_state",
        format!(
            "if(mn_xy_blocked = 1, toInt32(mn_xy_exploded.{state}), mn_state0)",
            state = stopped::STATE,
        ),
    );
    value(
        "mn_after_xy_tics",
        format!(
            "if(mn_xy_blocked = 1, toInt32(mn_xy_exploded.{tics}), mn_tics0)",
            tics = stopped::TICS,
        ),
    );
    value(
        "mn_after_xy_flags",
        format!(
            "if(mn_xy_blocked = 1, toInt32(mn_xy_exploded.{flags}), mn_flags0)",
            flags = stopped::FLAGS,
        ),
    );

    // P_ZMovement, run whenever the thing stands off its floor or still
    // carries momentum on it, whether or not the XY step above already
    // set it off.
    value(
        "mn_z_gate",
        "toUInt8(mn_after_xy_removed = 0 \
         AND (mn_z0 != mn_after_xy_floorz OR mn_after_xy_momz != 0))"
            .to_owned(),
    );
    value("mn_z_stepped", "mn_z0 + mn_after_xy_momz".to_owned());
    value(
        "mn_z_onfloor",
        "toUInt8(mn_z_gate = 1 AND mn_z_stepped <= mn_after_xy_floorz)".to_owned(),
    );
    value(
        "mn_z_landed",
        "if(mn_z_onfloor = 1, mn_after_xy_floorz, mn_z_stepped)".to_owned(),
    );
    value(
        "mn_z_momz_floor",
        "if(mn_z_onfloor = 1 AND mn_after_xy_momz < 0, toInt64(0), mn_after_xy_momz)".to_owned(),
    );
    value(
        "mn_z_floor_explodes",
        format!(
            "toUInt8(mn_z_onfloor = 1 AND bitAnd(mn_after_xy_flags, {missile}) != 0 \
             AND bitAnd(mn_after_xy_flags, {noclip}) = 0)",
            missile = MF_MISSILE,
            noclip = MF_NOCLIP,
        ),
    );
    value(
        "mn_z_hits_ceiling",
        "toUInt8(mn_z_gate = 1 AND mn_z_floor_explodes = 0 \
         AND mn_z_landed + mn_height > mn_after_xy_ceilingz)"
            .to_owned(),
    );
    value(
        "mn_z_momz_ceiling",
        "if(mn_z_hits_ceiling = 1 AND mn_z_momz_floor > 0, toInt64(0), mn_z_momz_floor)".to_owned(),
    );
    value(
        "mn_z_z_ceiling",
        "if(mn_z_hits_ceiling = 1, mn_after_xy_ceilingz - mn_height, mn_z_landed)".to_owned(),
    );
    value(
        "mn_z_ceiling_explodes",
        format!(
            "toUInt8(mn_z_hits_ceiling = 1 AND bitAnd(mn_after_xy_flags, {missile}) != 0 \
             AND bitAnd(mn_after_xy_flags, {noclip}) = 0)",
            missile = MF_MISSILE,
            noclip = MF_NOCLIP,
        ),
    );
    value(
        "mn_z_explode_needed",
        "toUInt8(mn_z_floor_explodes = 1 OR mn_z_ceiling_explodes = 1)".to_owned(),
    );
    value(
        "mn_z_explode_asks",
        format!(
            "if(mn_z_explode_needed = 1, [(toUInt32(1), toInt32(-1), \
             mn_base + mn_hit.{hit_draws} + mn_hurt.{hurt_draws})], \
             CAST([] AS Array(Tuple(UInt32, Int32, UInt32))))",
            hit_draws = struck::DRAWS,
            hurt_draws = inter::hurt::DRAWS,
        ),
    );
    // `explode_fold` reads a missile's own fields at a slot into an array.
    // The Z step's fields are what the XY step above already left rather
    // than the mobj arrays' own, so each is wrapped as the one-element
    // array a slot of 1 reads back.
    let after_xy = Flying {
        m_z: flying.m_z,
        m_height: flying.m_height,
        m_type: "[mn_type]",
        m_state: "[mn_after_xy_state]",
        m_tics: "[mn_after_xy_tics]",
        m_flags: "[mn_after_xy_flags]",
        m_target: flying.m_target,
        m_momx: flying.m_momx,
        m_momy: flying.m_momy,
        m_momz: flying.m_momz,
        m_floorz: flying.m_floorz,
        m_ceilingz: flying.m_ceilingz,
        m_subsector: flying.m_subsector,
        prndindex: flying.prndindex,
    };
    value(
        "mn_z_exploded",
        explode_fold("mn_z_explode_asks", &after_xy),
    );

    value(
        "mn_pre_momx",
        "if(mn_z_explode_needed = 1, toInt64(0), mn_after_xy_momx)".to_owned(),
    );
    value(
        "mn_pre_momy",
        "if(mn_z_explode_needed = 1, toInt64(0), mn_after_xy_momy)".to_owned(),
    );
    value(
        "mn_pre_momz",
        "if(mn_z_explode_needed = 1, toInt64(0), \
         if(mn_z_gate = 1, if(mn_z_hits_ceiling = 1, mn_z_momz_ceiling, mn_z_momz_floor), \
         mn_after_xy_momz))"
            .to_owned(),
    );
    value(
        "mn_pre_z",
        "if(mn_z_gate = 1, \
         if(mn_z_floor_explodes = 1, mn_z_landed, mn_z_z_ceiling), mn_z0)"
            .to_owned(),
    );
    value(
        "mn_pre_state",
        format!(
            "if(mn_z_explode_needed = 1, toInt32(mn_z_exploded.{state}), mn_after_xy_state)",
            state = stopped::STATE,
        ),
    );
    value(
        "mn_pre_tics",
        format!(
            "if(mn_z_explode_needed = 1, toInt32(mn_z_exploded.{tics}), mn_after_xy_tics)",
            tics = stopped::TICS,
        ),
    );
    value(
        "mn_pre_flags",
        format!(
            "if(mn_z_explode_needed = 1, toInt32(mn_z_exploded.{flags}), mn_after_xy_flags)",
            flags = stopped::FLAGS,
        ),
    );

    // The state cycle. `P_MobjThinker` runs this whether or not the steps
    // above just set a fresh death frame, so a missile that explodes this
    // tic has that frame's own tics decremented once more immediately.
    // `tics = -1` never decrements and never transitions; no missile
    // carries it, but the mover this runs for later might.
    value(
        "mn_cycle_tics",
        "if(mn_pre_tics = -1, -1, mn_pre_tics - 1)".to_owned(),
    );
    value(
        "mn_cycle_transitions",
        "toUInt8(mn_pre_tics != -1 AND mn_cycle_tics = 0)".to_owned(),
    );
    value(
        "mn_next_state",
        "toInt32(state_nextstate[1 + mn_pre_state])".to_owned(),
    );
    value(
        "mn_cycle_removes",
        "toUInt8(mn_cycle_transitions = 1 AND mn_next_state = 0)".to_owned(),
    );
    value(
        "mn_cycle_action_guard",
        "toUInt8(mn_cycle_transitions = 1 AND mn_next_state != 0 \
         AND state_action[1 + mn_next_state] != 0)"
            .to_owned(),
    );
    value(
        "mn_final_state",
        "if(mn_cycle_transitions = 1 AND mn_next_state != 0, mn_next_state, mn_pre_state)"
            .to_owned(),
    );
    value(
        "mn_final_tics",
        "if(mn_cycle_transitions = 1 AND mn_next_state != 0, \
         toInt32(state_tics[1 + mn_next_state]), mn_cycle_tics)"
            .to_owned(),
    );

    value(
        "mn_stuck",
        format!(
            "toUInt8(mn_guard = 1 OR (mn_moving = 1 AND mn_split = 1) \
             OR mn_after_xy_removed = 1 OR (mn_crossed = 1 AND mn_crossed_ok = 0) \
             OR (mn_xy_blocked = 1 AND mn_xy_exploded.{stuck} = 1) \
             OR (mn_z_explode_needed = 1 AND mn_z_exploded.{stuck} = 1) \
             OR mn_hurt.{hurt_stuck} = 1 OR mn_cycle_removes = 1 OR mn_cycle_action_guard = 1)",
            stuck = stopped::STUCK,
            hurt_stuck = inter::hurt::STUCK,
        ),
    );
    value(
        "mn_draws",
        format!(
            "toUInt32(mn_base + mn_hit.{hit_draws} + mn_hurt.{hurt_draws} \
             + mn_xy_exploded.{xy_draws} + mn_z_exploded.{z_draws})",
            hit_draws = struck::DRAWS,
            hurt_draws = inter::hurt::DRAWS,
            xy_draws = stopped::DRAWS,
            z_draws = stopped::DRAWS,
        ),
    );

    let members = [
        "toInt32(mn_after_xy_x)".to_owned(),
        "toInt32(mn_after_xy_y)".to_owned(),
        "toInt32(mn_pre_z)".to_owned(),
        "toInt32(mn_after_xy_floorz)".to_owned(),
        "toInt32(mn_after_xy_ceilingz)".to_owned(),
        "toInt32(mn_after_xy_subsector)".to_owned(),
        "toInt32(mn_pre_momx)".to_owned(),
        "toInt32(mn_pre_momy)".to_owned(),
        "toInt32(mn_pre_momz)".to_owned(),
        "toInt32(mn_final_state)".to_owned(),
        "toInt32(mn_final_tics)".to_owned(),
        "toInt32(mn_pre_flags)".to_owned(),
        "mn_hurt_target".to_owned(),
        "mn_hurt".to_owned(),
        "mn_draws".to_owned(),
        "mn_stuck".to_owned(),
    ];
    (
        values,
        format!("(({}), mn_hurt, mn_drop_asks)", members.join(", ")),
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
            m_momx: "m_momx",
            m_momy: "m_momy",
            m_momz: "m_momz",
            m_floorz: "m_floorz",
            m_ceilingz: "m_ceilingz",
            m_subsector: "m_subsector",
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
        let names: Vec<String> = SPECIES.iter().map(|name| name.to_lowercase()).collect();
        let bound: Vec<String> = constants("nat").into_iter().map(|(name, _)| name).collect();
        let sql = impact("asks", &flying());
        for name in names {
            assert!(bound.contains(&name), "{name}");
            assert!(sql.contains(&name), "{name}");
        }
    }

    /// `P_CrossSpecialLine` returns at once for a non-player thing of one
    /// of [`NO_SPECIAL`]'s types, and each of those comes from `mobjtype`
    /// the same way. `MT_TROOPSHOT` is `mobj::constants`'s own binding.
    #[test]
    fn every_type_p_cross_special_line_exempts_comes_from_the_table() {
        let mut bound: Vec<String> = constants("nat").into_iter().map(|(name, _)| name).collect();
        bound.extend(
            super::super::mobj::constants("nat")
                .into_iter()
                .map(|(name, _)| name),
        );
        for name in NO_SPECIAL {
            assert!(bound.contains(&name.to_lowercase()), "{name}");
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

    fn hurting() -> inter::Hurting<'static> {
        inter::Hurting {
            m_x: "m_x",
            m_y: "m_y",
            m_z: "m_z",
            m_momx: "m_momx",
            m_momy: "m_momy",
            m_momz: "m_momz",
            m_reactiontime: "m_reactiontime",
            m_type: "m_type",
            m_state: "m_state",
            m_tics: "m_tics",
            m_flags: "m_flags",
            m_health: "m_health",
            m_height: "m_height",
            m_target: "m_target",
            m_threshold: "m_threshold",
            m_player: "m_player",
            m_subsector: "m_subsector",
            prndindex: "prndindex",
            readyweapon: "readyweapon",
            p_cheats: "p_cheats",
            p_powers: "p_powers",
            sec_special: "sec_special",
        }
    }

    fn thinks_sql() -> String {
        thinks_fold("asks", &inter::no_hurt(), &map(), &flying(), &hurting())
    }

    #[test]
    fn the_thinks_expression_balances_its_parentheses() {
        let sql = thinks_sql();
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0, "{sql}");
    }

    /// The move, the impact and the explosion each appear once however
    /// many missiles the fold is given.
    #[test]
    fn each_primitive_in_the_thinker_appears_once() {
        let sql = thinks_sql();
        assert_eq!(sql.matches("arrayMap(mv ->").count(), 1, "{sql}");
        assert_eq!(
            sql.matches("arrayFold((st_held, st_ask) ->").count(),
            1,
            "{sql}"
        );
        // Once for the XY-blocked explosion, once for the Z-stage one.
        assert_eq!(
            sql.matches("arrayFold((ex_held, ex_ask) ->").count(),
            2,
            "{sql}"
        );
        assert_eq!(
            sql.matches("arrayFold((dm_held, dm_ask) ->").count(),
            1,
            "{sql}"
        );
    }

    /// `bind::chain_in` turns every named value into a position in a
    /// tuple, so the state cycle's own names do not survive into the
    /// generated text; what does is the one state table lookup its single
    /// transition makes.
    #[test]
    fn the_state_cycle_makes_one_transition() {
        let sql = thinks_sql();
        assert_eq!(sql.matches("state_nextstate[1 + ").count(), 1, "{sql}");
    }

    /// A split move, a crossed special line a type does not clear, and a
    /// missile with no `MF_NOGRAVITY` are all refused rather than guessed.
    /// The names are gone by the time `bind::chain_in` is done with them,
    /// so this reads the constants and the table names that survive.
    #[test]
    fn the_thinker_refuses_what_it_does_not_model() {
        let sql = thinks_sql();
        assert!(
            sql.contains(&(MF_FLOAT | MF_SKULLFLY).to_string()),
            "the float/skullfly guard: {sql}"
        );
        assert!(
            sql.contains(&MF_NOGRAVITY.to_string()),
            "the no-gravity guard: {sql}"
        );
        assert!(
            sql.contains(&(MAXMOVE / 2).to_string()),
            "the split guard: {sql}"
        );
        for name in NO_SPECIAL {
            assert!(sql.contains(&name.to_lowercase()), "{name}: {sql}");
        }
    }

    /// A thing-blocked move reaches the sky check with no ceilingline, and
    /// one blocked by geometry or a line reads the move test's own.
    #[test]
    fn a_thing_blocked_move_reaches_no_ceilingline() {
        assert_eq!(
            move_ceilingline("h.1", "t"),
            format!("toInt32(if(h.1 = 1, -1, t.{}))", answer::CEILINGLINE)
        );
    }
}
