//! What a monster does about the player, from `p_enemy.c`.
//!
//! `A_Look` waits for the player to come into view and `A_Chase` walks
//! towards the one it found. Both run inside `P_SetMobjState`, so a thing
//! that wakes runs its first chase on the same tic.

use crate::sql::sim::map::{self, World, answer};
use crate::sql::{Statement, bind, fixed};

/// `p_local.h`: how close is close enough to react to something behind.
const MELEERANGE: i64 = 64 << 16;
/// `tables.h`
const ANG90: i64 = 0x4000_0000;
const ANG270: i64 = 0xC000_0000;
/// `tables.h`: an eighth of a turn, which is the step a chase turns by,
/// and the shift that turns a direction into an angle.
const ANG45: i64 = 0x2000_0000;
const DIRSHIFT: u32 = 29;
/// `p_enemy.c`: the four directions `P_NewChaseDir` picks an axis from,
/// and the ninth that a thing which cannot move takes. `dirtype_t`
/// declares the eight in turn from east, going anticlockwise.
const DI_EAST: i64 = 0;
const DI_NORTH: i64 = 2;
const DI_WEST: i64 = 4;
const DI_SOUTH: i64 = 6;
const DI_NODIR: i64 = 8;
/// `p_enemy.c`: how far off an axis the target has to be for the chase to
/// take that axis.
const CHASE_SLOP: i64 = 10 << 16;
/// `d_mode.h`: the skill a monster stops waiting out its move count on.
const SK_NIGHTMARE: i64 = 4;
/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
const MF_JUSTHIT: i64 = 64;
const MF_JUSTATTACKED: i64 = 128;
const MF_AMBUSH: i64 = 32;
const MF_SHADOW: i64 = 0x4_0000;
const MF_FLOAT: i64 = 0x4000;

/// The sounds `A_Look`'s switch picks between with a draw, as the names
/// `sounds.h` gives them.
///
/// The switch has two arms, one over three sounds and one over two, and
/// each draws once. No column carries the sound a thing played, so which
/// arm it was does not reach the state row and the two are one list here.
const SEE_SOUNDS: [&str; 5] = [
    "sfx_posit1",
    "sfx_posit2",
    "sfx_posit3",
    "sfx_bgsit1",
    "sfx_bgsit2",
];

/// The thing types `P_CheckMissileRange` switches on, as the names
/// `info.h` gives them. `MT_VILE` and `MT_UNDEAD` decide whether the
/// routine reaches its draw; the rest only shorten the distance.
const MISSILE_TYPES: [&str; 5] = ["MT_VILE", "MT_UNDEAD", "MT_CYBORG", "MT_SPIDER", "MT_SKULL"];

/// What stops the load: a name a routine switches on that the table it
/// reads does not carry. A draw left out moves every random number after
/// it and nothing else would say so.
pub fn guards(db: &str) -> Vec<Statement> {
    let missing = |what: &str, table: &str, names: &[&str]| {
        let quoted: Vec<String> = names.iter().map(|name| format!("'{name}'")).collect();
        Statement::sql(format!(
            "SELECT throwIf(count() != {}, '{what}')\nFROM {db}.{table}\nWHERE name IN ({})",
            names.len(),
            quoted.join(", ")
        ))
    };
    vec![
        missing(
            "A_Look: a sound it switches on is missing",
            "sfxenum",
            &SEE_SOUNDS,
        ),
        missing(
            "P_CheckMissileRange: a thing type it switches on is missing",
            "mobjtype",
            &MISSILE_TYPES,
        ),
    ]
}

/// The constants `A_Look` and `A_Chase` read.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let names: Vec<String> = SEE_SOUNDS.iter().map(|name| format!("'{name}'")).collect();
    let mut constants = vec![(
        "a_look_sounds".to_owned(),
        format!(
            "(SELECT groupArray(toInt32(id)) FROM {db}.sfxenum WHERE name IN ({}))",
            names.join(", ")
        ),
    )];
    for table in ["opposite", "diags", "xspeed", "yspeed"] {
        constants.push((
            format!("dir_{table}"),
            super::table_column(db, table, "value"),
        ));
    }
    for column in ["speed", "meleestate", "missilestate", "activesound"] {
        constants.push((
            format!("mobj_{column}"),
            super::table_column(db, "mobjinfo", column),
        ));
    }
    constants.push((
        "a_chase".to_owned(),
        format!("assumeNotNull((SELECT id FROM {db}.action_functions WHERE name = 'A_Chase'))"),
    ));
    for name in MISSILE_TYPES {
        constants.push((
            name.to_lowercase(),
            format!("toInt32(assumeNotNull((SELECT id FROM {db}.mobjtype WHERE name = '{name}')))"),
        ));
    }
    constants
}

/// Whether `A_Look` draws a random number for the sound a thing makes on
/// seeing the player.
///
/// The switch's default arm takes the sound as it stands and draws
/// nothing, and a thing with no see sound never reaches the switch.
pub fn see_sound_draws(seesound: &str) -> String {
    format!("toUInt8(has(a_look_sounds, toInt32({seesound})))")
}

/// `P_LookForPlayers` with `allaround` false: whether the actor takes the
/// player as its target.
///
/// One player is in the game, so every way out of the loop leaves
/// `lastlook` at that player's index and the walk itself changes nothing
/// else. What is left is the three tests: the player is alive, the actor
/// can see it, and it is not behind the actor's back further off than
/// `MELEERANGE`.
pub fn look_for_players(
    seen: &str,
    health: &str,
    x: &str,
    y: &str,
    angle: &str,
    player_x: &str,
    player_y: &str,
) -> String {
    let to_player = fixed::point_to_angle(
        &format!("toInt32(toInt64({player_x}) - toInt64({x}))"),
        &format!("toInt32(toInt64({player_y}) - toInt64({y}))"),
        "tantoangle",
    );
    let behind = format!(
        "toUInt32(bitAnd(toInt64({to_player}) - toInt64({angle}) + 4294967296, 4294967295))"
    );
    let distance = fixed::aprox_distance(
        &format!("toInt32(toInt64({player_x}) - toInt64({x}))"),
        &format!("toInt32(toInt64({player_y}) - toInt64({y}))"),
    );
    format!(
        "toUInt8({seen} = 1 AND {health} > 0 AND \
         (NOT ({behind} > {ANG90} AND {behind} < {ANG270}) \
         OR toInt64({distance}) <= {MELEERANGE}))"
    )
}

/// The index `P_LookForPlayers` leaves in `lastlook`.
///
/// The loop returns only where `playeringame[lastlook]` holds, and one
/// player is in the game, so it always stops on that one.
pub const LASTLOOK: i64 = 0;

/// Where each part of one mover's shape sits. The shape is what the walk
/// answers decide on their own, before any random number is read.
mod shape {
    /// Whether `P_NewChaseDir` runs at all.
    pub const NEWCHASE: usize = 1;
    /// Whether the direct route to the target is taken.
    pub const DIRECT: usize = 2;
    /// Whether one of the two axes or the old direction is walkable.
    pub const FIRST: usize = 3;
    /// Whether the search over the eight directions finds one.
    pub const LOOP: usize = 4;
    /// Whether the way back is walkable.
    pub const TURNAROUND: usize = 5;
    /// Whether the thing has an active sound, which is one draw a tic.
    pub const SOUND: usize = 6;
    /// Whether the tic could not be produced for this thing.
    pub const STUCK: usize = 7;
    pub const MOVEDIR: usize = 8;
    pub const TURN: usize = 9;
    /// The two axis directions, with the way back already cleared.
    pub const E1: usize = 10;
    pub const E2: usize = 11;
    pub const DIAG: usize = 12;
    pub const DELTAX: usize = 13;
    pub const DELTAY: usize = 14;
    /// The move count `A_Chase` decremented, which the walk starts from.
    pub const COUNT: usize = 15;
    /// Whether the direct route was tried, which it is even where it
    /// fails.
    pub const DIRECTTRIED: usize = 16;
    /// Whether `P_CheckMissileRange` reaches its draw for the distance.
    pub const MISSILEDRAW: usize = 17;
    /// The distance that draw is compared against, clamped the way the
    /// routine clamps it.
    pub const MISSILEDIST: usize = 18;
    /// Whether the missile check answers yes without drawing, which is
    /// the target having just hit the thing.
    pub const MISSILEHIT: usize = 19;
}

/// Where each field of one mover's answer sits.
pub mod chased {
    pub const X: usize = 1;
    pub const Y: usize = 2;
    pub const Z: usize = 3;
    pub const ANGLE: usize = 4;
    pub const MOVEDIR: usize = 5;
    pub const MOVECOUNT: usize = 6;
    pub const REACTIONTIME: usize = 7;
    pub const THRESHOLD: usize = 8;
    pub const FLOORZ: usize = 9;
    pub const CEILINGZ: usize = 10;
    pub const SUBSECTOR: usize = 11;
    /// How many random numbers the thing drew.
    pub const DRAWS: usize = 12;
    pub const STUCK: usize = 13;
    /// The state the attack sends the thing to, or -1 where it does not
    /// attack.
    pub const STATE: usize = 14;
    pub const FLAGS: usize = 15;
}

/// The state a chase reads, as the names the tic binds the arrays under.
pub struct Chasing<'a> {
    /// The slots whose state cycle entered a state carrying `A_Chase`,
    /// in list order.
    pub movers: &'a str,
    /// How many entries of the cycle carried `A_Chase`, by slot. A thing
    /// that reached two of them in one tic is one this does not run.
    pub entries: &'a str,
    /// How many draws `A_Look` made, by slot. A thing that wakes shouts
    /// before it chases, so its own look draw is behind it.
    pub shouts: &'a str,
    pub m_x: &'a str,
    pub m_y: &'a str,
    pub m_z: &'a str,
    pub m_angle: &'a str,
    pub m_radius: &'a str,
    pub m_height: &'a str,
    pub m_flags: &'a str,
    pub m_type: &'a str,
    pub m_health: &'a str,
    pub m_target: &'a str,
    pub m_movedir: &'a str,
    pub m_movecount: &'a str,
    pub m_reactiontime: &'a str,
    pub m_threshold: &'a str,
    pub m_floorz: &'a str,
    pub m_ceilingz: &'a str,
    pub m_subsector: &'a str,
    /// Whether the thing can see the target it holds, by slot, from the
    /// one sight call the tic makes.
    pub sees_target: &'a str,
    pub prndindex: &'a str,
}

/// How many directions a thing can walk in, which is how many moves the
/// walk asks about for each mover.
const DIRECTIONS: i64 = 8;

/// Where the fold holds the movers and their answers.
mod ran {
    pub const MOVERS: usize = 1;
    pub const CHASED: usize = 2;
}

/// `A_Chase` over every thing whose cycle entered a state carrying it.
///
/// `P_Move` and every `P_TryWalk` inside `P_NewChaseDir` ask the same
/// question of the same position, because the search stops at the first
/// direction that works and nothing moves until one does. So one call of
/// the move test answers for all eight directions of every mover, and what
/// is left is which of them the engine would have reached first.
///
/// The whole of it is the body of a fold over a list of one entry or none,
/// so a tic with nothing to chase does not pay for the move test. The body
/// reads the fold's own parameter, without which it would be evaluated
/// outside the fold anyway.
///
/// Two of the engine's own branches are never reached and are not
/// written. `netgame` is false, so the chase does not look for a new
/// target when the one it has is out of sight, and `fastparm` is false,
/// so the skill alone decides whether a thing waits its move count out.
///
/// Everything this cannot answer for leaves the tic unresolved: a thing
/// with no target or one that cannot be shot, a thing that just attacked,
/// a melee attack, a missile check that needs a draw, a floating thing, a
/// move that crosses a special line, and two movers close enough to see
/// each other, which the engine runs one after the other.
pub fn chase(state: &Chasing<'_>, world: &World<'_>) -> Vec<(String, String)> {
    let at = |array: &str, slot: &str| format!("{array}[{slot}]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("cf_movers", format!("cw_at.{}", ran::MOVERS));
    // Every direction of every mover, asked once. The engine walks them
    // one after another and stops at the first that works, and none of
    // them moves anything until it does, so they all start from where the
    // thing stands.
    let step = |axis: &str| {
        format!(
            "toInt64(mobj_speed[1 + {}]) * toInt64(dir_{axis}[1 + cf_dir])",
            at(state.m_type, "cf_k")
        )
    };
    value(
        "cf_asks",
        format!(
            "arrayFlatten(arrayMap(cf_k -> arrayMap(cf_dir -> {}, range({DIRECTIONS})), \
             cf_movers))",
            map::asking(
                "cf_k",
                &format!("toInt64({}) + {}", at(state.m_x, "cf_k"), step("xspeed")),
                &format!("toInt64({}) + {}", at(state.m_y, "cf_k"), step("yspeed")),
                &at(state.m_radius, "cf_k"),
                &at(state.m_height, "cf_k"),
                &at(state.m_z, "cf_k"),
                &at(state.m_flags, "cf_k"),
                "0",
            ),
        ),
    );
    value("cf_answers", map::try_moves("cf_asks", world));
    // One mover's eight answers, so the passes below read their own and
    // copy nothing per direction.
    value(
        "cf_walks",
        format!(
            "arrayMap(i -> arraySlice(cf_answers, 1 + (i - 1) * {DIRECTIONS}, {DIRECTIONS}), \
             arrayEnumerate(cf_movers))"
        ),
    );
    value(
        "cf_shape",
        format!(
            "arrayMap((k, w) -> ({}), cf_movers, cf_walks)",
            shape(state)
        ),
    );
    // A draw the tic has already made stands ahead of this thing's own:
    // every look the list made up to and including this slot, and every
    // chase before it. How many a chase makes is not known until its own
    // first draw is read, because a thing whose missile check answers yes
    // attacks and returns rather than walking, so the running index is a
    // fold rather than a sum over counts worked out in advance.
    value("cf_shouts", format!("arrayCumSum({})", state.shouts));
    let sh = |field: usize| format!("cf_shape[i].{field}");
    let base = "cf_shouts[cf_movers[i]] + fb.2";
    let missile = format!(
        "toInt64(rnd[1 + bitAnd(toUInt32({}) + {base} + 1, 255)])",
        state.prndindex
    );
    let attacked = format!(
        "toUInt8({} = 1 OR ({} = 1 AND {missile} >= {}))",
        sh(shape::MISSILEHIT),
        sh(shape::MISSILEDRAW),
        sh(shape::MISSILEDIST),
    );
    value(
        "cf_run",
        format!(
            "arrayFold((fb, i) -> (arrayPushBack(fb.1, toUInt32({base})), \
             toUInt32(fb.2 + {})), arrayEnumerate(cf_movers), \
             (CAST([], 'Array(UInt32)'), toUInt32(0)))",
            draws(&sh, &attacked),
        ),
    );
    value("cf_base", "cf_run.1".to_owned());
    value(
        "cf_chased",
        format!(
            "arrayMap((k, w, sh, base) -> ({}), cf_movers, cf_walks, cf_shape, cf_base)",
            chased(state)
        ),
    );

    let body = "(cf_movers, cf_chased)";
    let start = format!(
        "({}, CAST([], 'Array(Tuple({}))'))",
        state.movers,
        CHASED_TYPES.join(", ")
    );
    vec![
        (
            "cw".to_owned(),
            format!(
                "arrayFold((cw_at, cw_step) -> {}, range(least(length({}), 1)), {start})",
                bind::chain_in("cf", &values, body),
                state.movers
            ),
        ),
        ("cw_chased".to_owned(), format!("cw.{}", ran::CHASED)),
        // Two movers the engine runs one after the other cannot both read
        // the world as it stood, so a tic holding a pair close enough to
        // change what the other is told is one this does not run. It asks
        // the move test nothing, so it stays outside the fold.
        (
            "cw_crowded".to_owned(),
            format!(
                "toUInt8(arrayExists((a, i) -> arrayExists(b -> {}, \
                 arraySlice({movers}, i + 1)), {movers}, arrayEnumerate({movers})))",
                crowded(state),
                movers = state.movers
            ),
        ),
    ]
}

/// The type of one mover's answer, in the order [`chased`] names it. The
/// fold starts from an empty list of them, and a list has to carry its
/// type.
const CHASED_TYPES: [&str; 15] = [
    "Int32", "Int32", "Int32", "UInt32", "Int32", "Int32", "Int32", "Int32", "Int32", "Int32",
    "Int32", "UInt32", "UInt8", "Int32", "Int32",
];

/// Whether one mover's move changes what another is told.
///
/// A thing reaches another's move test through `PIT_CheckThing`, which
/// stops at things closer than the two radii, and a monster picks nothing
/// up, so what it is told depends on nothing further off than that plus
/// what either of them can walk in a tic.
fn crowded(state: &Chasing<'_>) -> String {
    let axis = |array: &str| {
        format!(
            "abs(toInt64({array}[a]) - toInt64({array}[b])) < \
             toInt64({r}[a]) + toInt64({r}[b]) \
             + bitShiftLeft(toInt64(mobj_speed[1 + {t}[a]]) + toInt64(mobj_speed[1 + {t}[b]]), 16)",
            r = state.m_radius,
            t = state.m_type,
        )
    };
    format!("toUInt8({} AND {})", axis(state.m_x), axis(state.m_y))
}

/// What one mover's eight answers and its own state decide before any
/// random number is read, in the order [`shape`](shape) names them.
///
/// `k` is the slot, `w` its eight answers.
fn shape(state: &Chasing<'_>) -> String {
    let at = |array: &str| format!("{array}[k]");
    let target = format!("{}[k]", state.m_target);
    let walks = |dir: &str| format!("({dir} != {DI_NODIR} AND w[1 + {dir}].{} = 1)", answer::OK);
    let kind = |column: &str| format!("mobj_{column}[1 + {}]", at(state.m_type));
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value("cs_target", format!("toUInt32({target})"));
    value("cs_flags", format!("toInt64({})", at(state.m_flags)));
    value(
        "cs_reactiontime",
        format!(
            "toInt32(if({rt} != 0, {rt} - 1, {rt}))",
            rt = at(state.m_reactiontime)
        ),
    );
    value("cs_movedir", format!("toInt64({})", at(state.m_movedir)));
    value(
        "cs_turn",
        "toInt64(dir_opposite[1 + cs_movedir])".to_owned(),
    );
    value(
        "cs_deltax",
        format!(
            "toInt64({}[cs_target]) - toInt64({})",
            state.m_x,
            at(state.m_x)
        ),
    );
    value(
        "cs_deltay",
        format!(
            "toInt64({}[cs_target]) - toInt64({})",
            state.m_y,
            at(state.m_y)
        ),
    );
    value(
        "cs_d1",
        format!(
            "toInt64(multiIf(cs_deltax > {CHASE_SLOP}, {DI_EAST}, \
             cs_deltax < -{CHASE_SLOP}, {DI_WEST}, {DI_NODIR}))"
        ),
    );
    value(
        "cs_d2",
        format!(
            "toInt64(multiIf(cs_deltay < -{CHASE_SLOP}, {DI_SOUTH}, \
             cs_deltay > {CHASE_SLOP}, {DI_NORTH}, {DI_NODIR}))"
        ),
    );
    value(
        "cs_diag",
        "toInt64(dir_diags[1 + if(cs_deltay < 0, 2, 0) + if(cs_deltax > 0, 1, 0)])".to_owned(),
    );
    value("cs_e1", "toInt64(if(cs_d1 = cs_turn, 8, cs_d1))".to_owned());
    value("cs_e2", "toInt64(if(cs_d2 = cs_turn, 8, cs_d2))".to_owned());
    value(
        "cs_count",
        format!("toInt64({}) - 1", at(state.m_movecount)),
    );
    // `A_Chase`'s own `P_Move` runs only while the move count has not run
    // out, and `P_NewChaseDir` runs when it has or when that move fails.
    value(
        "cs_newchase",
        format!("toUInt8(NOT (cs_count >= 0 AND {}))", walks("cs_movedir")),
    );
    value(
        "cs_directtried",
        format!(
            "toUInt8(cs_newchase = 1 AND cs_d1 != {DI_NODIR} AND cs_d2 != {DI_NODIR} \
             AND cs_diag != cs_turn)"
        ),
    );
    value(
        "cs_direct",
        format!("toUInt8(cs_directtried = 1 AND {})", walks("cs_diag")),
    );
    value(
        "cs_first",
        format!(
            "toUInt8({} OR {} OR {})",
            walks("cs_e1"),
            walks("cs_e2"),
            walks("cs_movedir")
        ),
    );
    value(
        "cs_loop",
        format!(
            "toUInt8(arrayExists(d -> d != cs_turn AND w[1 + d].{} = 1, range({DIRECTIONS})))",
            answer::OK
        ),
    );
    value("cs_turnaround", format!("toUInt8({})", walks("cs_turn")));
    value("cs_sound", format!("toUInt8({} != 0)", kind("activesound")));
    value("cs_sight", format!("toUInt8({})", at(state.sees_target)));
    value(
        "cs_distance",
        format!(
            "toInt64({})",
            fixed::aprox_distance("toInt32(cs_deltax)", "toInt32(cs_deltay)")
        ),
    );
    // `P_CheckMeleeRange` measures first and only looks when the target is
    // close enough, so a distant target costs no line of sight. A melee
    // attack itself is not written.
    value(
        "cs_melee",
        format!(
            "toUInt8({} != 0 AND cs_distance < {} + toInt64({}[cs_target]) AND cs_sight = 1)",
            kind("meleestate"),
            MELEERANGE - (20 << 16),
            state.m_radius,
        ),
    );
    // `P_CheckMissileRange`. The engine measures from the actor to the
    // target the other way round from the melee test, and takes the same
    // answer, because the distance is an absolute one.
    value(
        "cs_missile_asked",
        format!(
            "toUInt8(cs_melee = 0 AND {} != 0 AND NOT (skill < {SK_NIGHTMARE} AND {} != 0))",
            kind("missilestate"),
            at(state.m_movecount)
        ),
    );
    value(
        "cs_missile_far",
        format!(
            "toInt64(bitShiftRight(cs_distance - {} - if({} = 0, {}, 0), 16))",
            64 << 16,
            kind("meleestate"),
            128 << 16,
        ),
    );
    // An archvile gives up beyond its own range and a revenant inside
    // its own, both before the draw, so what a thing is decides whether
    // the draw happens at all.
    value(
        "cs_missile_ranged",
        format!(
            "toUInt8(NOT ({t} = mt_vile AND cs_missile_far > {}) \
             AND NOT ({t} = mt_undead AND cs_missile_far < 196))",
            14 * 64,
            t = at(state.m_type),
        ),
    );
    // A revenant halves what is left of its own range, and a cyberdemon,
    // a spider mastermind and a lost soul halve theirs. Then the whole is
    // capped, and a cyberdemon's again.
    value(
        "cs_missile_halved",
        format!(
            "toInt64(least(if({t} = mt_undead OR {t} = mt_cyborg OR {t} = mt_spider \
             OR {t} = mt_skull, bitShiftRight(cs_missile_far, 1), cs_missile_far), 200))",
            t = at(state.m_type),
        ),
    );
    value(
        "cs_missile_dist",
        format!(
            "toInt64(if({t} = mt_cyborg AND cs_missile_halved > 160, 160, cs_missile_halved))",
            t = at(state.m_type),
        ),
    );
    value(
        "cs_missile_draw",
        format!(
            "toUInt8(cs_missile_asked = 1 AND cs_sight = 1 AND bitAnd(cs_flags, {MF_JUSTHIT}) = 0 \
             AND cs_reactiontime = 0 AND cs_missile_ranged = 1)"
        ),
    );
    // The target having just hit the thing is the one way the check
    // answers yes without reading a number.
    value(
        "cs_missile_hit",
        format!(
            "toUInt8(cs_missile_asked = 1 AND cs_sight = 1 AND bitAnd(cs_flags, {MF_JUSTHIT}) != 0)"
        ),
    );
    value(
        "cs_stuck",
        format!(
            "toUInt8({} != 1 \
             OR cs_target = 0 \
             OR bitAnd(toInt64({}[cs_target]), {MF_SHOOTABLE}) = 0 \
             OR bitAnd(cs_flags, {MF_JUSTATTACKED}) != 0 \
             OR bitAnd(cs_flags, {MF_FLOAT}) != 0 \
             OR bitAnd(toInt64({}[cs_target]), {MF_SHADOW}) != 0 \
             OR cs_melee = 1)",
            format_args!("{}[k]", state.entries),
            state.m_flags,
            state.m_flags,
        ),
    );

    let members = [
        "toUInt8(cs_newchase)".to_owned(),
        "toUInt8(cs_direct)".to_owned(),
        "toUInt8(cs_newchase = 1 AND cs_direct = 0 AND cs_first = 1)".to_owned(),
        "toUInt8(cs_newchase = 1 AND cs_direct = 0 AND cs_first = 0 AND cs_loop = 1)".to_owned(),
        "toUInt8(cs_newchase = 1 AND cs_direct = 0 AND cs_first = 0 AND cs_loop = 0 \
         AND cs_turnaround = 1)"
            .to_owned(),
        "toUInt8(cs_sound)".to_owned(),
        "toUInt8(cs_stuck)".to_owned(),
        "toInt64(cs_movedir)".to_owned(),
        "toInt64(cs_turn)".to_owned(),
        "toInt64(cs_e1)".to_owned(),
        "toInt64(cs_e2)".to_owned(),
        "toInt64(cs_diag)".to_owned(),
        "toInt64(cs_deltax)".to_owned(),
        "toInt64(cs_deltay)".to_owned(),
        "toInt64(cs_count)".to_owned(),
        "toUInt8(cs_directtried)".to_owned(),
        "toUInt8(cs_missile_draw)".to_owned(),
        "toInt64(cs_missile_dist)".to_owned(),
        "toUInt8(cs_missile_hit)".to_owned(),
    ];
    bind::chain_in("cs", &values, &format!("({})", members.join(", ")))
}

/// How many random numbers one mover draws.
///
/// `P_CheckMissileRange` draws once for the distance where it gets that
/// far. `P_NewChaseDir` draws once for the swap unless the direct route
/// carried it, once more for the direction the search runs in, and once
/// for the move count whenever a direction works. Which of the two axes
/// the swap puts first changes the order the search runs in and not
/// whether one of them works, so the count does not depend on either
/// number.
fn draws(field: &dyn Fn(usize) -> String, attacked: &str) -> String {
    format!(
        "toUInt32({} + if({attacked} = 1, 0, \
         multiIf({} = 0, 0, {} = 1, 1, {} = 1, 2, {} = 1 OR {} = 1, 3, 2) + {}))",
        field(shape::MISSILEDRAW),
        field(shape::NEWCHASE),
        field(shape::DIRECT),
        field(shape::FIRST),
        field(shape::LOOP),
        field(shape::TURNAROUND),
        field(shape::SOUND),
    )
}

/// What one mover's chase leaves behind, in the order [`chased`] names it.
///
/// `k` is the slot, `w` its eight answers, `sh` its shape and `base` how
/// many numbers the tic drew before it.
fn chased(state: &Chasing<'_>) -> String {
    let at = |array: &str| format!("{array}[k]");
    let sh = |field: usize| format!("sh.{field}");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    // `P_Random` reads the table at the index it has just moved on to.
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + base + {nth} + 1, 255)])",
            state.prndindex
        )
    };
    // The missile check draws before the walk does, so the walk's own
    // draws sit behind it.
    value("cc_missile_draw", draw("0"));
    value("cc_walk_at", format!("toUInt32(sh.{})", shape::MISSILEDRAW));
    value("cc_swap_draw", draw("cc_walk_at"));
    value("cc_search_draw", draw("cc_walk_at + 1"));
    // `P_CheckMissileRange` answers no when the number it drew is under
    // the distance. Where it answers yes, `A_Chase` puts the thing in its
    // missile frames and returns, so nothing below this runs.
    value(
        "cc_attacked",
        format!(
            "toUInt8(sh.{} = 1 OR (sh.{} = 1 AND cc_missile_draw >= sh.{}))",
            shape::MISSILEHIT,
            shape::MISSILEDRAW,
            shape::MISSILEDIST,
        ),
    );
    value("cc_draws", draws(&sh, "cc_attacked"));
    value(
        "cc_count_draw",
        draw(&format!("cc_draws - toUInt32(sh.{}) - 1", shape::SOUND)),
    );
    value(
        "cc_swap",
        format!(
            "toUInt8(cc_swap_draw > 200 OR abs({}) > abs({}))",
            sh(shape::DELTAY),
            sh(shape::DELTAX)
        ),
    );
    value(
        "cc_order",
        format!(
            "arrayConcat([if(cc_swap = 1, {e2}, {e1}), if(cc_swap = 1, {e1}, {e2}), {dir}], \
             arrayFilter(d -> d != {turn}, if(bitAnd(cc_search_draw, 1) != 0, \
             range({DIRECTIONS}), arrayReverse(range({DIRECTIONS})))), [{turn}])",
            e1 = sh(shape::E1),
            e2 = sh(shape::E2),
            dir = sh(shape::MOVEDIR),
            turn = sh(shape::TURN),
        ),
    );
    value(
        "cc_won",
        format!(
            "toInt64(arrayFirstIndex(d -> d != {DI_NODIR} AND w[1 + d].{} = 1, cc_order))",
            answer::OK
        ),
    );
    // Which direction the thing walked, and 8 for one that did not move.
    value(
        "cc_dir",
        format!(
            "toInt64(multiIf({} = 0, {dir}, {} = 1, {diag}, cc_won = 0, {DI_NODIR}, \
             cc_order[cc_won]))",
            sh(shape::NEWCHASE),
            sh(shape::DIRECT),
            dir = sh(shape::MOVEDIR),
            diag = sh(shape::DIAG),
        ),
    );
    value(
        "cc_moved",
        format!(
            "toUInt8(cc_attacked = 0 AND multiIf({} = 0, 1, {} = 1, 1, cc_won != 0))",
            sh(shape::NEWCHASE),
            sh(shape::DIRECT)
        ),
    );
    value(
        "cc_answer",
        "w[1 + if(cc_moved = 1, cc_dir, toInt64(0))]".to_owned(),
    );
    // The directions the engine reached, which is the move count's own
    // move, the direct route, and the search up to the one that worked.
    value(
        "cc_tried",
        format!(
            "arrayConcat(if({count} >= 0, [{dir}], CAST([], 'Array(Int64)')), \
             if({newchase} = 0, CAST([], 'Array(Int64)'), \
             arrayConcat(if({tried} = 1, [{diag}], CAST([], 'Array(Int64)')), \
             arraySlice(cc_order, 1, if(cc_won > 0, cc_won, toInt64(length(cc_order)))))))",
            count = sh(shape::COUNT),
            dir = sh(shape::MOVEDIR),
            newchase = sh(shape::NEWCHASE),
            tried = sh(shape::DIRECTTRIED),
            diag = sh(shape::DIAG),
        ),
    );
    // A move that crosses a special line runs it, and a blocked one that
    // reached one opens it. Neither is written.
    value(
        "cc_special",
        format!(
            "toUInt8(arrayExists(d -> d != {DI_NODIR} AND notEmpty(w[1 + d].{}), cc_tried))",
            answer::SPECHIT
        ),
    );
    // The direction the walk ends on. `P_NewChaseDir` leaves `DI_NODIR`
    // where nothing worked, and `A_Chase`'s own move leaves what it had.
    value(
        "cc_movedir",
        format!(
            "toInt64(multiIf(cc_attacked = 1, {}, {} = 0, {}, cc_dir))",
            sh(shape::MOVEDIR),
            sh(shape::NEWCHASE),
            sh(shape::MOVEDIR)
        ),
    );
    value(
        "cc_movecount",
        format!(
            "toInt64(multiIf(cc_attacked = 1, {}, {} = 0, {count}, \
             cc_moved = 0, {count}, bitAnd(cc_count_draw, 15)))",
            at(state.m_movecount),
            sh(shape::NEWCHASE),
            count = sh(shape::COUNT),
        ),
    );
    value(
        "cc_reactiontime",
        format!(
            "toInt32(if({rt} != 0, {rt} - 1, {rt}))",
            rt = at(state.m_reactiontime)
        ),
    );
    value(
        "cc_threshold",
        format!(
            "toInt32(multiIf({held} = 0, {held}, {} = 0 OR {}[{}[k]] <= 0, 0, {held} - 1))",
            format_args!("{}[k]", state.m_target),
            state.m_health,
            state.m_target,
            held = at(state.m_threshold),
        ),
    );
    // The turn towards the direction the thing is already walking in.
    value(
        "cc_facing",
        format!(
            "toInt64(bitAnd(toInt64({}), {}))",
            at(state.m_angle),
            7 * ANG45
        ),
    );
    value(
        "cc_delta",
        format!(
            "toInt64(bitAnd(cc_facing - bitShiftLeft({}, {DIRSHIFT}) + 4294967296, 4294967295))",
            sh(shape::MOVEDIR)
        ),
    );
    value(
        "cc_turned",
        format!(
            "toUInt32(bitAnd(cc_facing + multiIf(cc_delta = 0, 0, \
             cc_delta < 2147483648, -{ANG45}, {ANG45}) + 4294967296, 4294967295))"
        ),
    );
    // `A_FaceTarget` points the thing at what it is about to attack and
    // takes it off ambush. A target carrying `MF_SHADOW` turns the angle
    // by a random amount instead, which is two draws, and `cs_stuck`
    // refuses that tic. No target of a face-target on this map carries the
    // flag, because monsters face the player and the player has no blur
    // sphere.
    let target = |array: &str| format!("{array}[{}[k]]", state.m_target);
    value(
        "cc_faced",
        format!(
            "toUInt32({})",
            fixed::point_to_angle(
                &format!(
                    "toInt64({}) - toInt64({})",
                    target(state.m_x),
                    at(state.m_x)
                ),
                &format!(
                    "toInt64({}) - toInt64({})",
                    target(state.m_y),
                    at(state.m_y)
                ),
                "tantoangle",
            )
        ),
    );
    value(
        "cc_angle",
        format!(
            "toUInt32(multiIf(cc_attacked = 1, cc_faced, {} < {DI_NODIR}, cc_turned, {}))",
            sh(shape::MOVEDIR),
            at(state.m_angle)
        ),
    );

    let members = [
        format!(
            "toInt32(if(cc_moved = 1, toInt64({}) + toInt64(mobj_speed[1 + {}]) \
             * toInt64(dir_xspeed[1 + cc_dir]), toInt64({})))",
            at(state.m_x),
            at(state.m_type),
            at(state.m_x)
        ),
        format!(
            "toInt32(if(cc_moved = 1, toInt64({}) + toInt64(mobj_speed[1 + {}]) \
             * toInt64(dir_yspeed[1 + cc_dir]), toInt64({})))",
            at(state.m_y),
            at(state.m_type),
            at(state.m_y)
        ),
        format!(
            "toInt32(if(cc_moved = 1, cc_answer.{}, toInt64({})))",
            answer::FLOORZ,
            at(state.m_z)
        ),
        "toUInt32(cc_angle)".to_owned(),
        "toInt32(cc_movedir)".to_owned(),
        "toInt32(cc_movecount)".to_owned(),
        "toInt32(cc_reactiontime)".to_owned(),
        "toInt32(cc_threshold)".to_owned(),
        format!(
            "toInt32(if(cc_moved = 1, cc_answer.{}, toInt64({})))",
            answer::FLOORZ,
            at(state.m_floorz)
        ),
        format!(
            "toInt32(if(cc_moved = 1, cc_answer.{}, toInt64({})))",
            answer::CEILINGZ,
            at(state.m_ceilingz)
        ),
        format!(
            "toInt32(if(cc_moved = 1, cc_answer.{}, toInt64({})))",
            answer::SUBSECTOR,
            at(state.m_subsector)
        ),
        "toUInt32(cc_draws)".to_owned(),
        format!("toUInt8({} = 1 OR cc_special = 1)", sh(shape::STUCK)),
        format!(
            "toInt32(if(cc_attacked = 1, mobj_missilestate[1 + {}], -1))",
            at(state.m_type)
        ),
        // `P_CheckMissileRange` clears the mark on the branch that answers
        // yes without drawing, and `A_FaceTarget` takes the thing off
        // ambush as it turns.
        format!(
            "toInt32(if(cc_attacked = 1, bitOr(bitAnd(toInt64({flags}), \
             if(sh.{} = 1, {}, {})), {MF_JUSTATTACKED}), toInt64({flags})))",
            shape::MISSILEHIT,
            !(MF_AMBUSH | MF_JUSTHIT),
            !MF_AMBUSH,
            flags = at(state.m_flags),
        ),
    ];
    bind::chain_in("cc", &values, &format!("({})", members.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The switch draws for the five sounds it picks between and for
    /// nothing else, and the load stops if one of them is not in the
    /// table, because a draw left out moves every random number after it.
    #[test]
    fn the_sound_switch_draws_for_the_five_it_picks_between() {
        let (name, set) = constants("nat").into_iter().next().expect("the set");
        assert_eq!(name, "a_look_sounds");
        for sound in SEE_SOUNDS {
            assert_eq!(set.matches(sound).count(), 1, "{set}");
        }
        assert_eq!(
            see_sound_draws("s"),
            "toUInt8(has(a_look_sounds, toInt32(s)))"
        );
        let guard = &guards("nat")[0].sql;
        assert!(guard.contains("count() != 5"), "{guard}");
        for sound in SEE_SOUNDS {
            assert!(guard.contains(sound), "{guard}");
        }
    }

    fn chasing() -> Chasing<'static> {
        Chasing {
            movers: "mt_movers",
            entries: "mt_entries",
            shouts: "mt_shouts",
            m_x: "w_x",
            m_y: "w_y",
            m_z: "w_z",
            m_angle: "w_angle",
            m_radius: "w_radius",
            m_height: "w_height",
            m_flags: "w_flags",
            m_type: "w_type",
            m_health: "w_health",
            m_target: "w_target",
            m_movedir: "w_movedir",
            m_movecount: "w_movecount",
            m_reactiontime: "w_reactiontime",
            m_threshold: "w_threshold",
            m_floorz: "w_floorz",
            m_ceilingz: "w_ceilingz",
            m_subsector: "w_subsector",
            sees_target: "w_sees_target",
            prndindex: "w_prndindex",
        }
    }

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

    /// The whole of the chase is the body of a fold over a list of one
    /// entry or none, and the body reads the fold's own parameter. A body
    /// that read neither parameter would be evaluated outside the fold
    /// whatever the fold did, and the move test would cost every tic.
    #[test]
    fn the_chase_is_one_fold_over_a_list_of_one_entry_or_none() {
        let bindings = chase(&chasing(), &world());
        let (_, fold) = bindings
            .iter()
            .find(|(name, _)| name == "cw")
            .expect("the fold");
        assert!(fold.starts_with("arrayFold((cw_at, cw_step) ->"), "{fold}");
        assert!(
            fold.contains("range(least(length(mt_movers), 1))"),
            "{fold}"
        );
        assert!(
            fold.contains(&format!("cw_at.{}", ran::MOVERS)),
            "the body reads the fold's parameter"
        );
        assert_eq!(fold.matches("arrayMap(mv ->").count(), 1, "one move test");
        assert!(
            fold.contains("arrayMap(cf_dir -> (toUInt32(cf_k)"),
            "eight asks a mover"
        );
    }

    /// How many numbers a thing draws is a function of the walk's answers
    /// and of whether it attacks. `P_NewChaseDir`'s two draws decide the
    /// order the axes and the search run in, and neither decides whether
    /// one of them works; the missile check's own draw decides whether the
    /// thing attacks and returns, which is the difference between one draw
    /// and four, and the count names that answer rather than reading the
    /// number again.
    #[test]
    fn the_draw_count_names_the_attack_rather_than_the_number() {
        let count = draws(&|field| format!("sh.{field}"), "cc_attacked");
        assert!(count.contains("cc_attacked"), "{count}");
        assert!(!count.contains("rnd"), "{count}");
        for member in [
            shape::NEWCHASE,
            shape::DIRECT,
            shape::FIRST,
            shape::LOOP,
            shape::TURNAROUND,
            shape::SOUND,
        ] {
            assert!(count.contains(&format!("sh.{member}")), "{count}");
        }
    }

    #[test]
    fn the_chase_balances_its_parentheses() {
        for (name, expr) in chase(&chasing(), &world()) {
            let depth = expr.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{name}");
        }
    }
}
