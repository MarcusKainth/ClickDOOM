//! What a monster does when its state cycle reaches an attack, from
//! `p_enemy.c`.
//!
//! `A_TroopAttack` and `A_SargAttack` share a shape: face the target, ask
//! whether it is in reach, and either claw it or, for the imp, throw a
//! fireball. `A_Scream`, `A_Pain`, `A_XScream` and `A_Fall` leave a flag
//! or nothing at all.

use crate::sql::Statement;
use crate::sql::{bind, fixed};

/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `tables.h`
const ANGLE_WRAP: i64 = 1 << 32;
/// `p_enemy.c`: what the fuzz a `MF_SHADOW` target draws is shifted by.
const FACE_SHIFT: u32 = 21;
/// `p_local.h`: how far a claw reaches, and how far short of it
/// `P_CheckMeleeRange` stops.
const MELEERANGE: i64 = 64 * FRACUNIT;
const MELEE_SLOP: i64 = 20 * FRACUNIT;
/// `p_mobj.h`
const MF_SOLID: i64 = 2;
const MF_AMBUSH: i64 = 32;
const MF_SHADOW: i64 = 0x4_0000;

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
    for name in ["A_TroopAttack", "A_SargAttack"] {
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
    pub m_angle: &'a str,
    pub m_flags: &'a str,
    pub m_type: &'a str,
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

/// What one attack works out, as the values a body reads and the
/// [`attacked`] tuple it answers with.
fn attacks(world: &Attacking<'_>) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("ak_ask.{field}");
    let at = |array: &str| format!("{array}[ak_slot]");
    let on = |array: &str| format!("{array}[ak_target]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({}) + {nth}, 255)])",
            world.prndindex,
            a(striking::BASE),
        )
    };
    let across = |array: &str| format!("toInt32(toInt64({}) - toInt64({}))", on(array), at(array));

    value("ak_slot", format!("toUInt32({})", a(striking::SLOT)));
    value("ak_target", format!("toUInt32({})", at(world.m_target)));
    value("ak_runs", "toUInt8(ak_target != 0)".to_owned());
    // `A_FaceTarget`: the angle at the target, with the fuzz a `MF_SHADOW`
    // one draws, and the ambush flag off.
    value(
        "ak_fuzzy",
        format!(
            "toUInt8(ak_runs = 1 AND bitAnd({}, {MF_SHADOW}) != 0)",
            on(world.m_flags)
        ),
    );
    value(
        "ak_aimed",
        fixed::point_to_angle(&across(world.m_x), &across(world.m_y), "tantoangle"),
    );
    value(
        "ak_angle",
        format!(
            "toUInt32(bitAnd(toInt64(ak_aimed) \
             + if(ak_fuzzy = 1, bitShiftLeft({} - {}, {FACE_SHIFT}), 0) + {ANGLE_WRAP}, {}))",
            draw("1"),
            draw("2"),
            ANGLE_WRAP - 1,
        ),
    );
    // `P_CheckMeleeRange`, which the sight answer finishes.
    value(
        "ak_near",
        format!(
            "toUInt8(ak_runs = 1 AND toInt64({}) < {MELEERANGE} - {MELEE_SLOP} \
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
            draw("1 + 2 * toUInt32(ak_fuzzy)"),
            draw("1 + 2 * toUInt32(ak_fuzzy)"),
        ),
    );
    let members = [
        format!("toUInt32(if(ak_runs = 1, ak_angle, {}))", at(world.m_angle),),
        format!(
            "toInt32(if(ak_runs = 1, bitAnd({held}, {}), {held}))",
            !MF_AMBUSH,
            held = at(world.m_flags),
        ),
        "toUInt8(ak_near)".to_owned(),
        "toInt32(ak_damage)".to_owned(),
        "toUInt8(ak_runs = 1 AND ak_near = 0 AND ak_routine = a_troopattack)".to_owned(),
        "toUInt32(if(ak_runs = 0, 0, 2 * toUInt32(ak_fuzzy) + toUInt32(ak_near)))".to_owned(),
        "toUInt8(ak_runs = 1 AND ak_routine != a_troopattack AND ak_routine != a_sargattack)"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables;

    fn world() -> Attacking<'static> {
        Attacking {
            m_x: "m_x",
            m_y: "m_y",
            m_angle: "m_angle",
            m_flags: "m_flags",
            m_type: "m_type",
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
                "toUInt32(if(ak_runs = 0, 0, 2 * toUInt32(ak_fuzzy) + toUInt32(ak_near)))"
            ),
            "{body}"
        );
        assert!(named("ak_damage").contains("1 + 2 * toUInt32(ak_fuzzy)"));
    }

    /// `P_CheckMeleeRange` measures against the target's own type rather
    /// than the radius it is standing with.
    #[test]
    fn the_reach_is_measured_against_the_type_s_own_radius() {
        assert!(
            named("ak_near").contains("mobj_radius[1 + m_type[ak_target]]"),
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

    /// The two routines and the sounds come from the tables inside the
    /// statement rather than out of the generator.
    #[test]
    fn every_name_it_switches_on_comes_from_a_table() {
        let names: Vec<String> = constants("nat").into_iter().map(|(name, _)| name).collect();
        assert!(names.contains(&"a_troopattack".to_owned()), "{names:?}");
        assert!(names.contains(&"a_sargattack".to_owned()), "{names:?}");
        let sql = attack("asks", &world());
        assert!(
            sql.contains("a_troopattack") && sql.contains("a_sargattack"),
            "{sql}"
        );
        assert!(scream_draws("s").contains("a_scream_sounds"));
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
