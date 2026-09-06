//! What a monster does when its state cycle reaches an attack, from
//! `p_enemy.c`.
//!
//! `A_TroopAttack` and `A_SargAttack` share a shape: face the target, ask
//! whether it is in reach, and either claw it or, for the imp, throw a
//! fireball. `A_Scream`, `A_Pain`, `A_XScream` and `A_Fall` leave a flag
//! or nothing at all.

use super::{inter, maputl, shoot, sight};
use crate::sql::Statement;
use crate::sql::{bind, fixed};

/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `tables.h`
const ANGLE_WRAP: i64 = 1 << 32;
/// `p_enemy.c`: what the fuzz a `MF_SHADOW` target draws is shifted by.
const FACE_SHIFT: u32 = 21;
/// `p_local.h`: how far a claw reaches, how far short of it
/// `P_CheckMeleeRange` stops, and how far a bullet reaches.
const MELEERANGE: i64 = 64 * FRACUNIT;
const MELEE_SLOP: i64 = 20 * FRACUNIT;
const MISSILERANGE: i64 = 32 * 64 * FRACUNIT;
/// `p_enemy.c`: what the fuzz a shot's own angle takes is shifted by.
const SPREAD_SHIFT: u32 = 20;
/// `p_mobj.c`: how many numbers `P_SpawnPuff` and `P_SpawnBlood` draw
/// between them, which every shot that reaches something pays.
const DEBRIS_DRAWS: i64 = 4;
/// `p_local.h`: the furthest a thing's edge reaches from the cell its
/// origin sits in.
const MAXRADIUS: i64 = 32 * FRACUNIT;
/// `p_enemy.c`: what `A_Explode` asks `P_RadiusAttack` for, and nothing
/// else in the engine calls it.
const BOMBDAMAGE: i64 = 128;
/// `p_map.c`: how far from the spot the block walk reaches.
///
/// The engine works this out as `(damage + MAXRADIUS) << FRACBITS` in an
/// `int`. `MAXRADIUS` is already in fixed point, so its term is two to the
/// thirty-seventh and the shift carries it off the top of the word. What is
/// left is the damage in fixed point, and the walk covers 128 units rather
/// than the 160 the expression reads as.
const BLAST_REACH: i64 = (((BOMBDAMAGE + MAXRADIUS) << 16) as i32) as i64;

/// `p_mobj.h`
const MF_SOLID: i64 = 2;
const MF_SHOOTABLE: i64 = 4;
const MF_AMBUSH: i64 = 32;
const MF_SHADOW: i64 = 0x4_0000;

/// The thing types `PIT_RadiusAttack` names by hand, which take no damage
/// from a blast. `enemy::constants` binds both for `P_CheckMissileRange`.
const BOSSES: [&str; 2] = ["MT_CYBORG", "MT_SPIDER"];

/// The sounds `A_Scream`'s switch picks between with a draw, as the names
/// `sounds.h` gives them.
///
/// The switch has two arms, one over three sounds and one over two, and
/// each draws once. No column carries the sound a thing made, so which arm
/// it was does not reach the state row and the two are one list here.
const DEATH_SOUNDS: [&str; 5] = [
    "sfx_podth1",
    "sfx_podth2",
    "sfx_podth3",
    "sfx_bgdth1",
    "sfx_bgdth2",
];

/// What stops the load: a name a routine switches on that the table it
/// reads does not carry. A draw left out moves every random number after
/// it and nothing else would say so.
pub fn guards(db: &str) -> Vec<Statement> {
    let names: Vec<String> = DEATH_SOUNDS
        .iter()
        .map(|name| format!("'{name}'"))
        .collect();
    vec![Statement::sql(format!(
        "SELECT throwIf(count() != {}, \
         'A_Scream switches on a sound sounds.h does not carry')\n     \
         FROM {db}.sfxenum WHERE name IN ({})",
        DEATH_SOUNDS.len(),
        names.join(", "),
    ))]
}

/// The engine tables an attack reads that no other stage does.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let names: Vec<String> = DEATH_SOUNDS
        .iter()
        .map(|name| format!("'{name}'"))
        .collect();
    let mut constants = vec![
        (
            "a_scream_sounds".to_owned(),
            format!(
                "(SELECT groupArray(toInt32(id)) FROM {db}.sfxenum WHERE name IN ({}))",
                names.join(", ")
            ),
        ),
        (
            "mobj_deathsound".to_owned(),
            super::table_column(db, "mobjinfo", "deathsound"),
        ),
    ];
    for name in [
        "A_TroopAttack",
        "A_SargAttack",
        "A_Explode",
        "A_PosAttack",
        "A_SPosAttack",
    ] {
        constants.push((
            name.to_lowercase(),
            format!("assumeNotNull((SELECT id FROM {db}.action_functions WHERE name = '{name}'))"),
        ));
    }
    constants
}

/// Whether `A_Scream` draws a random number for the sound a thing makes as
/// it dies.
///
/// The switch's default arm takes the sound as it stands and draws
/// nothing, and a thing with no death sound returns before the switch.
pub fn scream_draws(deathsound: &str) -> String {
    format!("toUInt8(has(a_scream_sounds, toInt32({deathsound})))")
}

/// `A_Fall`: the thing stops being something to walk into.
pub fn fallen(flags: &str) -> String {
    format!("toInt32(bitAnd({flags}, {}))", !MF_SOLID)
}

/// The mobj arrays an attack reads.
pub struct Attacking<'a> {
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_z: &'a str,
    pub m_angle: &'a str,
    pub m_height: &'a str,
    pub m_flags: &'a str,
    pub m_type: &'a str,
    pub m_health: &'a str,
    pub m_target: &'a str,
    pub prndindex: &'a str,
}

/// Where each field of an attack ask sits in its tuple.
pub mod striking {
    /// The slot attacking.
    pub const SLOT: usize = 1;
    /// The `action_functions` id of the routine the frame carries.
    pub const ROUTINE: usize = 2;
    /// Whether the attacker can see its target, from the one sight call
    /// the tic makes.
    pub const SEES: usize = 3;
    /// How many numbers the tic drew before this call's own.
    pub const BASE: usize = 4;
}

/// Where each field of what an attack leaves sits in its tuple.
pub mod attacked {
    pub const ANGLE: usize = 1;
    pub const FLAGS: usize = 2;
    /// 1 where the claw reached, which is what does the damage.
    pub const CLAWED: usize = 3;
    /// What the claw does, 0 where it did not reach.
    pub const DAMAGE: usize = 4;
    /// 1 where the routine threw a missile instead.
    pub const THROWS: usize = 5;
    /// How many numbers the call drew.
    pub const DRAWS: usize = 6;
    /// 1 where the call reached a path this does not write.
    pub const STUCK: usize = 7;
}

/// `A_TroopAttack` and `A_SargAttack` over every ask in `asks`, as an
/// [`attacked`] tuple each.
///
/// Both run `A_FaceTarget` and then `P_CheckMeleeRange`. A `MF_SHADOW`
/// target makes the face draw twice, and a claw that reaches draws once
/// more; an imp whose claw does not reach throws a fireball instead, which
/// the caller spawns.
///
/// `P_CheckMeleeRange` ends in `P_CheckSight`, which the tic asks once for
/// every pair it needs, so the answer comes in with the ask.
pub fn attack(asks: &str, world: &Attacking<'_>) -> String {
    let (values, body) = attacks(world);
    format!(
        "arrayMap(ak_ask -> {}, {asks})",
        bind::chain_in("aka", &values, &body)
    )
}

/// [`attack`] over an ask list that carries at most one, folded rather
/// than mapped.
///
/// A map runs every function in its body once even on an empty list, and
/// this body is the whole routine. A fold runs its body only where the
/// list has an element, so a tic reaching no attack pays for the fold and
/// nothing under it. The answer is the last ask in the list, and
/// [`no_attack`] is what an empty one gives.
pub fn attack_fold(asks: &str, world: &Attacking<'_>) -> String {
    let (values, body) = attacks(world);
    format!(
        "arrayFold((ak_held, ak_ask) -> {}, {asks}, {})",
        bind::chain_in("aka", &values, &body),
        no_attack(),
    )
}

/// The [`attacked`] tuple for a tic that reached no attack: no turn, no
/// claw, no missile and no draw.
pub fn no_attack() -> String {
    "(toUInt32(0), toInt32(0), toUInt8(0), toInt32(0), toUInt8(0), toUInt32(0), toUInt8(0))"
        .to_owned()
}

/// `A_FaceTarget`, as the values a routine that begins with it reads.
///
/// `slot` names the thing attacking and `base` how many numbers the tic
/// drew before the call. The values are `fc_slot`, `fc_target`, `fc_runs`,
/// `fc_angle` and `fc_flags`, plus `fc_fuzzy`, which is 1 where the target
/// carries `MF_SHADOW` and the angle is turned by the first two numbers the
/// call draws. A thing with no target runs none of it.
fn face_target(slot: &str, base: &str, world: &Attacking<'_>) -> Vec<(String, String)> {
    let at = |array: &str| format!("{array}[fc_slot]");
    let on = |array: &str| format!("{array}[fc_target]");
    let across = |array: &str| format!("toInt32(toInt64({}) - toInt64({}))", on(array), at(array));
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({base}) + {nth}, 255)])",
            world.prndindex,
        )
    };
    vec![
        ("fc_slot".to_owned(), format!("toUInt32({slot})")),
        (
            "fc_target".to_owned(),
            format!("toUInt32({})", at(world.m_target)),
        ),
        ("fc_runs".to_owned(), "toUInt8(fc_target != 0)".to_owned()),
        (
            "fc_fuzzy".to_owned(),
            format!(
                "toUInt8(fc_runs = 1 AND bitAnd({}, {MF_SHADOW}) != 0)",
                on(world.m_flags)
            ),
        ),
        (
            "fc_aimed".to_owned(),
            fixed::point_to_angle(&across(world.m_x), &across(world.m_y), "tantoangle"),
        ),
        (
            "fc_angle".to_owned(),
            format!(
                "toUInt32(if(fc_runs = 0, {held}, bitAnd(toInt64(fc_aimed) \
                 + if(fc_fuzzy = 1, bitShiftLeft({} - {}, {FACE_SHIFT}), 0) + {ANGLE_WRAP}, {})))",
                draw("1"),
                draw("2"),
                ANGLE_WRAP - 1,
                held = at(world.m_angle),
            ),
        ),
        (
            "fc_flags".to_owned(),
            format!(
                "toInt32(if(fc_runs = 1, bitAnd({held}, {}), {held}))",
                !MF_AMBUSH,
                held = at(world.m_flags),
            ),
        ),
    ]
}

/// What one attack works out, as the values a body reads and the
/// [`attacked`] tuple it answers with.
fn attacks(world: &Attacking<'_>) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("ak_ask.{field}");
    let at = |array: &str| format!("{array}[fc_slot]");
    let on = |array: &str| format!("{array}[fc_target]");
    let mut values: Vec<(String, String)> =
        face_target(&a(striking::SLOT), &a(striking::BASE), world);
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({}) + {nth}, 255)])",
            world.prndindex,
            a(striking::BASE),
        )
    };
    let across = |array: &str| format!("toInt32(toInt64({}) - toInt64({}))", on(array), at(array));

    // `P_CheckMeleeRange`, which the sight answer finishes.
    value(
        "ak_near",
        format!(
            "toUInt8(fc_runs = 1 AND toInt64({}) < {MELEERANGE} - {MELEE_SLOP} \
             + toInt64(mobj_radius[1 + {}]) AND {} = 1)",
            fixed::aprox_distance(&across(world.m_x), &across(world.m_y)),
            on(world.m_type),
            a(striking::SEES),
        ),
    );
    value("ak_routine", format!("toInt32({})", a(striking::ROUTINE)));
    // The claw. The imp's is three times the draw and the demon's four
    // times a wider one.
    value(
        "ak_damage",
        format!(
            "toInt32(if(ak_near = 0, 0, multiIf(\
             ak_routine = a_troopattack, ({} % 8 + 1) * 3, \
             ak_routine = a_sargattack, ({} % 10 + 1) * 4, 0)))",
            draw("1 + 2 * toUInt32(fc_fuzzy)"),
            draw("1 + 2 * toUInt32(fc_fuzzy)"),
        ),
    );
    let members = [
        "toUInt32(fc_angle)".to_owned(),
        "toInt32(fc_flags)".to_owned(),
        "toUInt8(ak_near)".to_owned(),
        "toInt32(ak_damage)".to_owned(),
        "toUInt8(fc_runs = 1 AND ak_near = 0 AND ak_routine = a_troopattack)".to_owned(),
        "toUInt32(if(fc_runs = 0, 0, 2 * toUInt32(fc_fuzzy) + toUInt32(ak_near)))".to_owned(),
        "toUInt8(fc_runs = 1 AND ak_routine != a_troopattack AND ak_routine != a_sargattack)"
            .to_owned(),
    ];
    (values, format!("({})", members.join(", ")))
}

/// The `inter::hurting` ask a claw makes: the target, with the attacker as
/// both the inflictor and the source.
///
/// `attacked` names one [`attacked`] tuple, `slot` the attacker. `base` is
/// how many numbers the tic drew before the attack's own, so the damage
/// call's draws sit behind the ones the attack made.
pub fn claw_ask(attacked: &str, slot: &str, m_target: &str, base: &str) -> String {
    format!(
        "(toUInt32({m_target}[{slot}]), toUInt32({slot}), toUInt32({slot}), \
         toInt32({attacked}.{damage}), toUInt32({base}) + toUInt32({attacked}.{draws}))",
        damage = attacked::DAMAGE,
        draws = attacked::DRAWS,
    )
}

// ---------------------------------------------------------------------------
// A_Explode
// ---------------------------------------------------------------------------

/// Where each field of a blast ask sits in its tuple.
pub mod bombing {
    /// The slot the blast goes off at, which is the inflictor.
    pub const SPOT: usize = 1;
    /// The slot the blast is credited to, 0 for none. `A_Explode` passes
    /// the thing's own target.
    pub const SOURCE: usize = 2;
    /// How many numbers the tic drew before this call's own.
    pub const BASE: usize = 3;
}

/// Where each field of what a blast decides sits in its tuple.
pub mod bombed {
    /// The damage calls the blast makes, as [`inter::hurting`] asks, in the
    /// order the block walk reaches them and each with its own base.
    pub const ASKS: usize = 1;
    /// How many numbers those calls draw between them.
    pub const DRAWS: usize = 2;
}

/// The ClickHouse type of a [`bombed`] tuple, for a caller that carries a
/// list of them through a fold.
pub const BOMBED_TYPE: &str = "Tuple(Array(Tuple(UInt32, UInt32, UInt32, Int32, UInt32)), UInt32)";

/// The mobj arrays a blast reads.
pub struct Blast<'a> {
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_z: &'a str,
    pub m_radius: &'a str,
    pub m_height: &'a str,
    pub m_flags: &'a str,
    pub m_type: &'a str,
    pub m_subsector: &'a str,
    pub m_linkseq: &'a str,
    /// One per mobj slot: 1 while it is still on the list.
    pub alive: &'a str,
}

/// `A_Explode`, which is `P_RadiusAttack` at the thing, credited to
/// whatever the thing was chasing, for 128 damage.
pub fn blast_ask(slot: &str, m_target: &str, base: &str) -> String {
    format!("(toUInt32({slot}), toUInt32({m_target}[{slot}]), toUInt32({base}))")
}

/// `P_RadiusAttack` over every ask in `asks`, as a [`bombed`] tuple each.
///
/// The walk covers a square of blockmap cells around the spot, one row at a
/// time and left to right inside a row, which is the order the engine's two
/// loops reach them; inside a cell it reaches the thing linked last first.
/// `PIT_RadiusAttack` skips a thing that cannot be shot and the two bosses
/// concussion does not reach, measures the distance as the wider of the two
/// axes less the thing's own radius in whole units held at zero, and skips
/// one that far or further away. What is left takes `128 - dist`, and only
/// where it can see the spot.
///
/// The thing the blast goes off at is not skipped by name. `P_KillMobj` has
/// already taken `MF_SHOOTABLE` off it by the time its death frame runs
/// `A_Explode`, so the shootable test is what leaves it out.
///
/// The asks come back with their bases already counted: nothing a damage
/// call draws changes how many draws it makes, so [`inter::draws`] answers
/// every count before any call is worked out.
///
/// The whole of it is the body of a fold, so it runs once per ask and not
/// at all for none. It asks `P_CheckSight` about the things the walk
/// reaches, and reads the seg openings [`sight::seg_openings`] binds.
pub fn radius_attack(asks: &str, world: &Blast<'_>, hurting: &inter::Hurting<'_>) -> String {
    let (values, body) = bombs(world, hurting);
    format!(
        "arrayFold((rd_held, rd_ask) -> arrayPushBack(rd_held, {}), {asks}, \
         CAST([], 'Array({BOMBED_TYPE})'))",
        bind::chain_in("rda", &values, &body)
    )
}

/// What one blast works out, as the values a body reads and the [`bombed`]
/// tuple it answers with.
fn bombs(world: &Blast<'_>, hurting: &inter::Hurting<'_>) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("rd_ask.{field}");
    let at = |array: &str| format!("{array}[rd_spot]");
    let on = |array: &str, slot: &str| format!("{array}[{slot}]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("rd_spot", format!("toUInt32({})", a(bombing::SPOT)));
    value("rd_source", format!("toUInt32({})", a(bombing::SOURCE)));
    // The square of cells, row by row. `P_BlockThingsIterator` answers
    // nothing for a cell off the map, which is what the clamp does.
    let side = |origin: &str, count: &str, coord: &str| {
        format!(
            "range(greatest(bitShiftRight(toInt64({coord}) - {BLAST_REACH} - {origin}, {shift}), 0), \
             least(bitShiftRight(toInt64({coord}) + {BLAST_REACH} - {origin}, {shift}), \
             {count} - 1) + 1)",
            shift = maputl::MAPBLOCKSHIFT,
        )
    };
    value(
        "rd_cells",
        format!(
            "arrayFlatten(arrayMap(by -> arrayMap(bx -> by * bmap_cols + bx, {}), {}))",
            side("bmap_orgx", "bmap_cols", &at(world.m_x)),
            side("bmap_orgy", "bmap_rows", &at(world.m_y)),
        ),
    );
    value(
        "rd_reached",
        format!(
            "arraySort(k -> (indexOf(rd_cells, {cell}), -toInt64({})), \
             arrayFilter(k -> {alive}[k] = 1 AND has(rd_cells, {cell}), arrayEnumerate({alive})))",
            on(world.m_linkseq, "k"),
            cell = maputl::cell_of("k", world.m_x, world.m_y),
            alive = world.alive,
        ),
    );
    // `PIT_RadiusAttack` up to the sight check, as the slot and what the
    // blast would do to it.
    let axis = |array: &str| format!("abs(toInt64({}) - toInt64({}))", on(array, "k"), at(array));
    value(
        "rd_scored",
        format!(
            "arrayMap(k -> (toUInt32(k), toInt32({BOMBDAMAGE} - greatest(bitShiftRight(\
             greatest({}, {}) - toInt64({}), 16), 0))), rd_reached)",
            axis(world.m_x),
            axis(world.m_y),
            on(world.m_radius, "k"),
        ),
    );
    let boss = BOSSES
        .iter()
        .map(|name| format!("{} != {}", on(world.m_type, "s.1"), name.to_lowercase()))
        .collect::<Vec<_>>()
        .join(" AND ");
    value(
        "rd_near",
        format!(
            "arrayFilter(s -> bitAnd({}, {MF_SHOOTABLE}) != 0 AND {boss} AND s.2 > 0, rd_scored)",
            on(world.m_flags, "s.1"),
        ),
    );
    // `P_CheckSight (thing, bombspot)`, in the walk's own order.
    value(
        "rd_pairs",
        format!(
            "arrayMap(s -> {}, rd_near)",
            sight::asking(
                &on(world.m_subsector, "s.1"),
                &on(world.m_x, "s.1"),
                &on(world.m_y, "s.1"),
                &on(world.m_z, "s.1"),
                &on(world.m_height, "s.1"),
                &at(world.m_subsector),
                &at(world.m_x),
                &at(world.m_y),
                &at(world.m_z),
                &at(world.m_height),
            )
        ),
    );
    value("rd_seen", sight::check_sight("rd_pairs"));
    value(
        "rd_hit",
        "arrayFilter((s, v) -> v = 1, rd_near, rd_seen)".to_owned(),
    );
    // Every call's own draw count, worked out before any of them is.
    value(
        "rd_counts",
        inter::draws(
            "arrayMap(s -> (s.1, rd_spot, rd_source, s.2, toUInt32(0)), rd_hit)",
            hurting,
        ),
    );
    value(
        "rd_bases",
        "arrayMap((c, d) -> toUInt32(c - d), arrayCumSum(rd_counts), rd_counts)".to_owned(),
    );
    let members = [
        format!(
            "arrayMap((s, b) -> (s.1, rd_spot, rd_source, s.2, toUInt32(toUInt32({}) + b)), \
             rd_hit, rd_bases)",
            a(bombing::BASE)
        ),
        "toUInt32(arraySum(rd_counts))".to_owned(),
    ];
    (values, format!("({})", members.join(", ")))
}

// ---------------------------------------------------------------------------
// A_PosAttack and A_SPosAttack
// ---------------------------------------------------------------------------

/// Where each field of a gunshot ask sits in its tuple.
pub mod gunning {
    /// The slot firing.
    pub const SLOT: usize = 1;
    /// The `action_functions` id of the routine the frame carries.
    pub const ROUTINE: usize = 2;
    /// How many numbers the tic drew before this call's own.
    pub const BASE: usize = 3;
}

/// Where each field of what a gunshot leaves sits in its tuple.
///
/// The four arrays run together, one entry per shot in the order they were
/// fired. A caller reads a shot's own answer out of [`SHOTS`] and hands the
/// two bases to `mobj::spawn_debris` and `inter::damage_mobj`.
pub mod gunned {
    pub const ANGLE: usize = 1;
    pub const FLAGS: usize = 2;
    /// The slope the one `P_AimLineAttack` found, which every shot leaves
    /// at.
    pub const SLOPE: usize = 3;
    /// What each shot reached, as a `shoot::reached` answer.
    pub const SHOTS: usize = 4;
    /// What each shot rolled.
    pub const DAMAGE: usize = 5;
    /// How many numbers the tic had drawn before each shot's own puff or
    /// blood spot.
    pub const SPAWN_BASE: usize = 6;
    /// How many it had drawn before each shot's damage call.
    pub const HURT_BASE: usize = 7;
    /// How many numbers the call drew, the shots' puffs and damage
    /// included.
    pub const DRAWS: usize = 8;
    /// 1 where the call reached a path this does not write.
    pub const STUCK: usize = 9;
}

/// Where the fold holds what the shots have left.
mod gun {
    pub const SLOPE: usize = 1;
    pub const SHOTS: usize = 2;
    pub const DAMAGE: usize = 3;
    pub const SPAWN_BASE: usize = 4;
    pub const HURT_BASE: usize = 5;
    pub const DRAWS: usize = 6;
    pub const STUCK: usize = 7;
}

/// `A_PosAttack` and `A_SPosAttack` over every ask in `asks`, as a
/// [`gunned`] tuple each.
///
/// Both run `A_FaceTarget`, take one slope from `P_AimLineAttack` at the
/// angle the face left, and then fire: one shot for the zombieman and three
/// for the shotgun guy. Each shot turns the angle by two numbers and rolls
/// its damage from a third, and the puff or blood spot it leaves draws four
/// more before the next shot turns its own angle.
///
/// The shots are a fold because how many numbers a shot draws depends on
/// what the shot before it reached. Step zero of that fold is the aim and
/// every step after it is a shot, so the trace, its intercepts and their
/// order are in the statement once.
///
/// The asks are a fold too, so a tic with nothing shooting runs none of
/// it.
pub fn hitscan(
    asks: &str,
    world: &Attacking<'_>,
    targets: &shoot::Targets<'_>,
    hurting: &inter::Hurting<'_>,
) -> String {
    let (values, body) = gunshots(world, targets, hurting);
    format!(
        "arrayFold((gn_held, gn_ask) -> arrayPushBack(gn_held, {}), {asks}, \
         CAST([], 'Array({})'))",
        bind::chain_in("gna", &values, &body),
        gunned_type(),
    )
}

/// The ClickHouse type of a [`gunned`] tuple, which the fold's empty list
/// has to carry.
fn gunned_type() -> String {
    format!(
        "Tuple(UInt32, Int32, Int32, Array({}), Array(Int32), Array(UInt32), Array(UInt32), \
         UInt32, UInt8)",
        shoot::REACHED_TYPE,
    )
}

/// What one gunshot works out, as the values a body reads and the
/// [`gunned`] tuple it answers with.
fn gunshots(
    world: &Attacking<'_>,
    targets: &shoot::Targets<'_>,
    hurting: &inter::Hurting<'_>,
) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("gn_ask.{field}");
    let mut values: Vec<(String, String)> =
        face_target(&a(gunning::SLOT), &a(gunning::BASE), world);
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("gn_routine", format!("toInt32({})", a(gunning::ROUTINE)));
    // The zombieman fires once and the shotgun guy three times.
    value(
        "gn_shots",
        "toInt64(multiIf(gn_routine = a_posattack, 1, gn_routine = a_sposattack, 3, 0))".to_owned(),
    );
    // What the face itself drew, which is where the first shot's own
    // numbers start.
    value(
        "gn_faced",
        "toUInt32(if(fc_runs = 0, 0, 2 * toUInt32(fc_fuzzy)))".to_owned(),
    );
    value(
        "gn_ran",
        gun_fold(&a(gunning::BASE), world, targets, hurting),
    );

    let held = |field: usize| format!("gn_ran.{field}");
    let members = [
        "toUInt32(fc_angle)".to_owned(),
        "toInt32(fc_flags)".to_owned(),
        format!("toInt32({})", held(gun::SLOPE)),
        held(gun::SHOTS),
        held(gun::DAMAGE),
        held(gun::SPAWN_BASE),
        held(gun::HURT_BASE),
        format!("toUInt32({})", held(gun::DRAWS)),
        format!(
            "toUInt8({} = 1 OR (fc_runs = 1 AND gn_shots = 0))",
            held(gun::STUCK)
        ),
    ];
    (values, format!("({})", members.join(", ")))
}

/// The aim and the shots, as one fold whose first step is the aim.
///
/// A thing with no target folds over nothing and the whole of the walk
/// costs the empty list.
fn gun_fold(
    base: &str,
    world: &Attacking<'_>,
    targets: &shoot::Targets<'_>,
    hurting: &inter::Hurting<'_>,
) -> String {
    let (values, body) = gun_step(base, world, targets, hurting);
    format!(
        "arrayFold((gs_at, gs_step) -> {}, range(if(fc_runs = 1, 1 + gn_shots, 0)), \
         (toInt32(0), CAST([], 'Array({})'), CAST([], 'Array(Int32)'), \
         CAST([], 'Array(UInt32)'), CAST([], 'Array(UInt32)'), toInt64(gn_faced), toUInt8(0)))",
        bind::chain_in("gsa", &values, &body),
        shoot::REACHED_TYPE,
    )
}

/// One step of the fold: the aim at step zero, and a shot at every step
/// after it.
fn gun_step(
    base: &str,
    world: &Attacking<'_>,
    targets: &shoot::Targets<'_>,
    hurting: &inter::Hurting<'_>,
) -> (Vec<(String, String)>, String) {
    let at = |array: &str| format!("{array}[fc_slot]");
    let held = |field: usize| format!("gs_at.{field}");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    // The numbers this step draws sit behind the ask's own and everything
    // the steps before it drew.
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({base}) + toUInt32({}) + {nth}, \
             255)])",
            world.prndindex,
            held(gun::DRAWS),
        )
    };

    value(
        "gs_angle",
        format!(
            "toUInt32(bitAnd(toInt64(fc_angle) + bitShiftLeft({} - {}, {SPREAD_SHIFT}) \
             + {ANGLE_WRAP}, {}))",
            draw("1"),
            draw("2"),
            ANGLE_WRAP - 1,
        ),
    );
    value("gs_damage", format!("toInt32(({} % 5 + 1) * 3)", draw("3")));
    // One walk serves the aim and the shots. Step zero asks the angle the
    // face left and every step after it asks the one that shot leaves at.
    let ask = |angle: &str, slope: &str, aims: bool| {
        let shooter = "fc_slot";
        let (x, y, z, height) = (
            at(world.m_x),
            at(world.m_y),
            at(world.m_z),
            at(world.m_height),
        );
        let range = MISSILERANGE.to_string();
        if aims {
            shoot::asking(shooter, &x, &y, &z, &height, angle, &range)
        } else {
            shoot::shooting(shooter, &x, &y, &z, &height, angle, &range, slope)
        }
    };
    // `shoot::traverse` names its list twice, so it is bound once here.
    value(
        "gs_ask",
        format!(
            "[if(gs_step = 0, {}, {})]",
            ask("fc_angle", "0", true),
            ask("gs_angle", &held(gun::SLOPE), false),
        ),
    );
    value(
        "gs_reached",
        format!("{}[1]", shoot::traverse("gs_ask", targets)),
    );
    value(
        "gs_kind",
        format!("toUInt8(gs_reached.{})", shoot::reached::KIND),
    );
    value(
        "gs_id",
        format!("toInt32(gs_reached.{})", shoot::reached::ID),
    );
    // `P_SpawnPuff` for a wall and for a thing that cannot bleed, and
    // `P_SpawnBlood` for one that can; both draw the same four numbers.
    value(
        "gs_spawn_draws",
        format!("toInt64(if(gs_kind = 0, 0, {DEBRIS_DRAWS}))"),
    );
    value(
        "gs_hurt_draws",
        format!(
            "toInt64(if(gs_kind != 2, 0, {}[1]))",
            inter::draws(
                "[(toUInt32(gs_id), toUInt32(fc_slot), toUInt32(fc_slot), gs_damage, \
                 toUInt32(0))]",
                hurting,
            )
        ),
    );
    // A shot that kills what it hit changes what a later shot of the same
    // call would reach, and a special line the shot crossed is
    // `P_ShootSpecialLine`, which this does not run.
    value(
        "gs_stuck",
        format!(
            "toUInt8(notEmpty(gs_reached.{}) \
             OR (gs_kind = 2 AND gs_step < gn_shots AND toInt64({}[gs_id]) - gs_damage <= 0))",
            shoot::reached::SPECHIT,
            world.m_health,
        ),
    );

    let body = format!(
        "if(gs_step = 0, (toInt32(gs_reached.{slope}), {shots}, {damage}, {spawn}, {hurt}, \
         {draws}, {stuck}), \
         ({}, arrayPushBack({shots}, gs_reached), arrayPushBack({damage}, gs_damage), \
         arrayPushBack({spawn}, toUInt32({draws} + 3)), \
         arrayPushBack({hurt}, toUInt32({draws} + 3 + gs_spawn_draws)), \
         toInt64({draws} + 3 + gs_spawn_draws + gs_hurt_draws), \
         toUInt8({stuck} = 1 OR gs_stuck = 1)))",
        held(gun::SLOPE),
        slope = shoot::reached::SLOPE,
        shots = held(gun::SHOTS),
        damage = held(gun::DAMAGE),
        spawn = held(gun::SPAWN_BASE),
        hurt = held(gun::HURT_BASE),
        draws = held(gun::DRAWS),
        stuck = held(gun::STUCK),
    );
    (values, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables;

    fn world() -> Attacking<'static> {
        Attacking {
            m_x: "m_x",
            m_y: "m_y",
            m_z: "m_z",
            m_angle: "m_angle",
            m_height: "m_height",
            m_flags: "m_flags",
            m_type: "m_type",
            m_health: "m_health",
            m_target: "m_target",
            prndindex: "prndindex",
        }
    }

    fn named(name: &str) -> String {
        let (values, _) = attacks(&world());
        values
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, expr)| expr.clone())
            .unwrap_or_else(|| panic!("the call names {name}"))
    }

    /// A thing with no target draws nothing, a fuzzy one moves the claw's
    /// draw behind the two the face makes, and a claw that does not reach
    /// draws nothing of its own.
    #[test]
    fn the_draws_follow_the_target_and_the_reach() {
        let (_, body) = attacks(&world());
        assert!(
            body.contains(
                "toUInt32(if(fc_runs = 0, 0, 2 * toUInt32(fc_fuzzy) + toUInt32(ak_near)))"
            ),
            "{body}"
        );
        assert!(named("ak_damage").contains("1 + 2 * toUInt32(fc_fuzzy)"));
    }

    /// `P_CheckMeleeRange` measures against the target's own type rather
    /// than the radius it is standing with.
    #[test]
    fn the_reach_is_measured_against_the_type_s_own_radius() {
        assert!(
            named("ak_near").contains("mobj_radius[1 + m_type[fc_target]]"),
            "{}",
            named("ak_near")
        );
    }

    /// A routine this does not write leaves the call stuck rather than
    /// guessed.
    #[test]
    fn a_routine_it_does_not_write_leaves_the_call_stuck() {
        let (_, body) = attacks(&world());
        assert!(
            body.contains("ak_routine != a_troopattack AND ak_routine != a_sargattack"),
            "{body}"
        );
    }

    /// `A_Fall` takes the one flag it takes and nothing else.
    #[test]
    fn a_fall_clears_the_solid_flag() {
        assert_eq!(fallen("f"), "toInt32(bitAnd(f, -3))");
    }

    /// No frame a damage call enters carries an `A_Scream` that draws.
    ///
    /// `P_KillMobj` enters the death frame and runs whatever it carries,
    /// and `inter.rs` allows `A_Scream` there without counting a draw for
    /// it. Every type whose death frame carries one has a death sound the
    /// switch takes as it stands. This fails if a table change gives one of
    /// them a sound the switch draws for.
    #[test]
    fn no_death_frame_a_damage_call_enters_screams_with_a_draw() {
        let states = tables::table("states").unwrap();
        let action = states.ints("action").unwrap();
        let actions = tables::table("action_functions").unwrap();
        let at = actions
            .texts("name")
            .unwrap()
            .iter()
            .position(|held| *held == "A_Scream")
            .expect("the engine carries the routine");
        let a_scream = actions.ints("id").unwrap()[at];
        let sounds = tables::table("sfxenum").unwrap();
        let names = sounds.texts("name").unwrap();
        let ids = sounds.ints("id").unwrap();
        let drawn: Vec<i64> = DEATH_SOUNDS
            .iter()
            .map(|name| {
                let at = names
                    .iter()
                    .position(|held| held == name)
                    .expect("the sound is in the table");
                ids[at]
            })
            .collect();
        let info = tables::table("mobjinfo").unwrap();
        let deathsound = info.ints("deathsound").unwrap();
        let mut met = 0;
        for column in ["painstate", "deathstate", "xdeathstate"] {
            for (kind, state) in info.ints(column).unwrap().into_iter().enumerate() {
                if state == 0 || action[state as usize] != a_scream {
                    continue;
                }
                met += 1;
                assert!(
                    !drawn.contains(&deathsound[kind]),
                    "type {kind} screams from a frame a damage call enters"
                );
            }
        }
        assert!(met > 0, "the tables carry such a frame");
    }

    /// The sounds `A_Scream` draws for are the ones `sounds.h` carries,
    /// which is what the load guard checks.
    #[test]
    fn every_sound_a_scream_switches_on_is_in_the_table() {
        let sounds = tables::table("sfxenum").unwrap();
        let names = sounds.texts("name").unwrap();
        for sound in DEATH_SOUNDS {
            assert!(names.contains(&sound), "{sound}");
        }
    }

    /// The routines and the sounds come from the tables inside the
    /// statement rather than out of the generator.
    #[test]
    fn every_name_it_switches_on_comes_from_a_table() {
        let names: Vec<String> = constants("nat").into_iter().map(|(name, _)| name).collect();
        assert!(names.contains(&"a_troopattack".to_owned()), "{names:?}");
        assert!(names.contains(&"a_sargattack".to_owned()), "{names:?}");
        assert!(names.contains(&"a_explode".to_owned()), "{names:?}");
        let sql = attack("asks", &world());
        assert!(
            sql.contains("a_troopattack") && sql.contains("a_sargattack"),
            "{sql}"
        );
        assert!(scream_draws("s").contains("a_scream_sounds"));
    }

    fn blast() -> Blast<'static> {
        Blast {
            m_x: "m_x",
            m_y: "m_y",
            m_z: "m_z",
            m_radius: "m_radius",
            m_height: "m_height",
            m_flags: "m_flags",
            m_type: "m_type",
            m_subsector: "m_subsector",
            m_linkseq: "m_linkseq",
            alive: "m_alive",
        }
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
            prndindex: "prndindex",
            readyweapon: "readyweapon",
        }
    }

    fn bombed_value(name: &str) -> String {
        let (values, _) = bombs(&blast(), &hurting());
        values
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, expr)| expr.clone())
            .unwrap_or_else(|| panic!("the call names {name}"))
    }

    /// The walk reaches 128 units, not the 160 the engine's expression
    /// reads as: `MAXRADIUS` is already in fixed point and the shift carries
    /// its term off the top of the word.
    #[test]
    fn the_blast_reaches_what_the_overflow_leaves() {
        assert_eq!(BLAST_REACH, BOMBDAMAGE << 16);
        assert_ne!(BLAST_REACH, (BOMBDAMAGE + MAXRADIUS) << 16);
        assert!(bombed_value("rd_cells").contains(&BLAST_REACH.to_string()));
    }

    /// `P_RadiusAttack` walks the rows outside and the columns inside,
    /// which decides the order the damage calls draw in.
    #[test]
    fn the_square_is_walked_row_by_row() {
        let cells = bombed_value("rd_cells");
        assert!(
            cells.starts_with("arrayFlatten(arrayMap(by -> arrayMap(bx ->"),
            "the rows are the outer loop: {cells}"
        );
        let cols = cells
            .find("bmap_cols - 1")
            .expect("the columns are clamped");
        let rows = cells.find("bmap_rows - 1").expect("the rows are clamped");
        assert!(cols < rows, "the inner range is the columns: {cells}");
    }

    /// Each call's base counts the draws of the calls before it and not its
    /// own.
    #[test]
    fn the_bases_are_the_prefix_of_the_counts() {
        assert_eq!(
            bombed_value("rd_bases"),
            "arrayMap((c, d) -> toUInt32(c - d), arrayCumSum(rd_counts), rd_counts)"
        );
    }

    /// A blast is its own inflictor and is credited to whatever the thing
    /// was chasing.
    #[test]
    fn the_blast_is_its_own_inflictor() {
        let (_, body) = bombs(&blast(), &hurting());
        assert!(body.contains("(s.1, rd_spot, rd_source, s.2,"), "{body}");
        assert_eq!(
            blast_ask("k", "m_target", "b"),
            "(toUInt32(k), toUInt32(m_target[k]), toUInt32(b))"
        );
        assert_eq!(inter::hurting::INFLICTOR, 2);
        assert_eq!(inter::hurting::SOURCE, 3);
    }

    /// The two bosses concussion does not reach come from `mobjtype`
    /// inside the statement rather than out of the generator.
    #[test]
    fn the_bosses_come_from_the_table() {
        let bound: Vec<String> = super::super::constants("nat")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let near = bombed_value("rd_near");
        for boss in BOSSES {
            let name = boss.to_lowercase();
            assert!(near.contains(&name), "the walk reads {name}");
            assert!(bound.contains(&name), "the statement binds {name}");
        }
    }

    /// The blast asks the sight check once however many things it reaches,
    /// and the whole of it is the body of a fold, so a tic with no barrel
    /// going off does not run it.
    #[test]
    fn the_sight_check_is_one_call_site_inside_a_fold() {
        let sql = radius_attack("asks", &blast(), &hurting());
        assert_eq!(
            sql.matches("sg_reject").count(),
            sight::check_sight("p").matches("sg_reject").count()
        );
        assert_eq!(sql.matches("arrayFold((rd_held, rd_ask) ->").count(), 1);
        assert!(!sql.contains("arrayMap(rd_ask ->"), "the body is a fold's");
    }

    fn gunned_value(name: &str) -> String {
        let (values, _) = gunshots(&world(), &targets(), &hurting());
        values
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, expr)| expr.clone())
            .unwrap_or_else(|| panic!("the call names {name}"))
    }

    fn targets() -> shoot::Targets<'static> {
        shoot::Targets {
            m_x: "m_x",
            m_y: "m_y",
            m_z: "m_z",
            m_radius: "m_radius",
            m_height: "m_height",
            m_flags: "m_flags",
            m_linkseq: "m_linkseq",
            alive: "m_alive",
            floorheight: "floorheight",
            ceilingheight: "ceilingheight",
            line_special: "line_special",
        }
    }

    /// The zombieman fires once and the shotgun guy three times.
    #[test]
    fn each_routine_fires_what_the_engine_fires() {
        let shots = gunned_value("gn_shots");
        assert!(shots.contains("gn_routine = a_posattack, 1"), "{shots}");
        assert!(shots.contains("gn_routine = a_sposattack, 3"), "{shots}");
    }

    /// One walk serves the aim and every shot, so the trace and its
    /// intercepts are in the statement once.
    #[test]
    fn one_walk_serves_the_aim_and_the_shots() {
        let sql = hitscan("asks", &world(), &targets(), &hurting());
        assert_eq!(
            sql.matches("arrayMap((am_ask, am_hits)").count(),
            1,
            "one walk"
        );
        assert_eq!(
            sql.matches("arrayFold((gs_at, gs_step)").count(),
            1,
            "one fold"
        );
    }

    fn step_value(name: &str) -> String {
        let (values, _) = gun_step("gn_ask.3", &world(), &targets(), &hurting());
        values
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, expr)| expr.clone())
            .unwrap_or_else(|| panic!("the step names {name}"))
    }

    /// A shot turns its angle by the first two numbers it draws, the
    /// earlier one on the left, and rolls its damage from the third.
    #[test]
    fn a_shot_draws_the_spread_before_the_damage() {
        let angle = step_value("gs_angle");
        let first = angle.find("+ 1, 255").expect("the first of the spread");
        let second = angle.find("+ 2, 255").expect("the second");
        assert!(
            first < second,
            "the earlier draw is the left operand: {angle}"
        );
        assert!(angle.contains(&format!("{SPREAD_SHIFT})")), "{angle}");
        let damage = step_value("gs_damage");
        assert!(damage.contains("+ 3, 255"), "{damage}");
        assert!(damage.contains("% 5 + 1) * 3"), "{damage}");
    }

    /// The aim draws nothing, so the first shot's numbers sit right behind
    /// the face's.
    #[test]
    fn the_aim_draws_nothing() {
        let (_, body) = gunshots(&world(), &targets(), &hurting());
        assert!(body.contains("toUInt32(fc_angle)"), "{body}");
        let faced = gunned_value("gn_faced");
        assert_eq!(
            faced,
            "toUInt32(if(fc_runs = 0, 0, 2 * toUInt32(fc_fuzzy)))"
        );
    }

    /// A shot's puff sits behind its own three numbers and its damage call
    /// behind the puff.
    #[test]
    fn the_bases_follow_the_engine_s_order() {
        let (_, body) = gun_step("gn_ask.3", &world(), &targets(), &hurting());
        assert!(
            body.contains("+ 3)"),
            "the puff sits behind the shot: {body}"
        );
        assert!(
            body.contains("+ 3 + gs_spawn_draws)"),
            "the damage sits behind the puff: {body}"
        );
        assert!(
            step_value("gs_spawn_draws").contains(&format!("gs_kind = 0, 0, {DEBRIS_DRAWS}")),
            "{}",
            step_value("gs_spawn_draws")
        );
    }

    /// A routine this does not write, and a shot that kills something a
    /// later shot of the same call would reach, leave the call stuck.
    #[test]
    fn what_it_cannot_answer_for_leaves_the_call_stuck() {
        let (_, body) = gunshots(&world(), &targets(), &hurting());
        assert!(body.contains("fc_runs = 1 AND gn_shots = 0"), "{body}");
        assert!(
            step_value("gs_stuck").contains("gs_step < gn_shots"),
            "{}",
            step_value("gs_stuck")
        );
    }

    #[test]
    fn the_gunshot_expression_balances_its_parentheses() {
        let sql = hitscan("asks", &world(), &targets(), &hurting());
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
    }

    #[test]
    fn the_blast_expression_balances_its_parentheses() {
        let sql = radius_attack("asks", &blast(), &hurting());
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0, "{sql}");
    }

    #[test]
    fn the_expression_balances_its_parentheses() {
        let sql = attack("asks", &world());
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0, "{sql}");
    }
}
