//! The player's weapon sprites, from `p_pspr.c`.
//!
//! `P_MovePsprites` runs the two sprites' state cycles inside
//! `P_PlayerThink`, and `P_SetPsprite` may enter several states in one
//! call because an action routine can put the sprite somewhere else. Both
//! are one fold: its list holds a step for each state entry each cycling
//! sprite is given, so a tic where neither cycles walks nothing.

use crate::sql::bind;
use crate::sql::fixed;

use super::{State, inter, maputl, mobj, shoot};

/// `p_pspr.c`
const LOWERSPEED: i64 = 6 << 16;
const RAISESPEED: i64 = 6 << 16;
const WEAPONBOTTOM: i64 = 128 << 16;
const WEAPONTOP: i64 = 32 << 16;
/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `tables.h`
const FINEMASK: i64 = 8191;
const FINEANGLES_HALF: i64 = 4095;
/// `d_event.h`
const BT_ATTACK: i64 = 1;
/// `doomdef.h`: `wp_nochange`.
const WP_NOCHANGE: i64 = 10;
/// `doomdef.h`: the two weapons that do not fire again while the button is
/// held, and the one whose shot costs two rounds.
const WP_MISSILE: i64 = 4;
const WP_BFG: i64 = 6;
const WP_SUPERSHOTGUN: i64 = 8;
/// `doomdef.h`: `am_noammo`, which a weapon that needs none carries.
const AM_NOAMMO: i64 = 5;
/// `p_pspr.c`: what one shot of the BFG costs.
const BFGCELLS: i64 = 40;
/// `d_player.h`: `PST_DEAD`.
const PST_DEAD: i64 = 2;
/// `p_pspr.h`: the two sprites, one-based for the arrays that hold both.
const PS_WEAPON: usize = 1;
const PS_FLASH: usize = 2;
/// A sprite with no state. `P_SetPsprite` stores a null pointer and the
/// probe writes -1 for it.
const NO_STATE: i64 = -1;

/// How many states one sprite's cycle may enter in a tic.
///
/// The entry the tic count runs out on is the first. Each routine written
/// here redirects at most once, and the chain cannot run past three:
/// `A_Raise` leaves the sprite at `WEAPONTOP`, which is too high for
/// `A_Lower` to finish on, and `A_Lower` leaves it at `WEAPONBOTTOM`,
/// which is too low for `A_Raise` to. The fourth is headroom. A cycle that
/// wants more says the tic could not be produced.
const ENTRIES: usize = 4;

/// Where each field of the fold's accumulator sits.
mod held {
    pub const STATE: usize = 1;
    pub const TICS: usize = 2;
    pub const SX: usize = 3;
    pub const SY: usize = 4;
    pub const READYWEAPON: usize = 5;
    pub const PENDINGWEAPON: usize = 6;
    pub const ATTACKDOWN: usize = 7;
    /// The state each sprite is about to enter, or -1 for none.
    pub const PENDING: usize = 8;
    pub const UNRESOLVED: usize = 9;
    /// Whether an entry ran `A_WeaponReady`, which takes the player's mobj
    /// out of its attack frames.
    pub const READIED: usize = 10;
    /// Whether an entry fired, which puts it back into them and sends the
    /// noise out.
    pub const FIRED: usize = 11;
    /// How many shots the entry's own routine sent down the barrel.
    pub const SHOTS: usize = 12;
    /// Whether those shots go where the weapon points rather than spread.
    pub const ACCURATE: usize = 13;
    /// The frame the flash sprite was put into, -1 for none.
    pub const FLASH: usize = 14;
    /// What the light routines have left, which the status bar and the
    /// renderer read as the flash's brightness.
    pub const EXTRALIGHT: usize = 15;
}

/// The constants the sprites read: the weapon table and the action
/// routines this dispatches on.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let weapon = |column: &str| super::table_column(db, "weaponinfo", column);
    let action = |name: &str| {
        format!("assumeNotNull((SELECT id FROM {db}.action_functions WHERE name = '{name}'))")
    };
    vec![
        ("weapon_upstate".to_owned(), weapon("upstate")),
        ("weapon_downstate".to_owned(), weapon("downstate")),
        ("weapon_readystate".to_owned(), weapon("readystate")),
        ("weapon_atkstate".to_owned(), weapon("atkstate")),
        ("weapon_ammo".to_owned(), weapon("ammo")),
        ("weapon_flashstate".to_owned(), weapon("flashstate")),
        ("a_weaponready".to_owned(), action("A_WeaponReady")),
        ("a_lower".to_owned(), action("A_Lower")),
        ("a_raise".to_owned(), action("A_Raise")),
        ("a_firepistol".to_owned(), action("A_FirePistol")),
        ("a_fireshotgun".to_owned(), action("A_FireShotgun")),
        ("a_light0".to_owned(), action("A_Light0")),
        ("a_light1".to_owned(), action("A_Light1")),
        ("a_light2".to_owned(), action("A_Light2")),
    ]
}

/// `P_MovePsprites`: the two sprites' cycles, then the flash sprite taking
/// the weapon's position.
///
/// `bob` is `player->bob` as `P_CalcHeight` left it, `buttons` the tic
/// command's, and `pendingweapon` the weapon the command asked for. The
/// stage names `now_p_readyweapon` and `psp_pendingweapon`, which the
/// pickups after it read, and `psp_readied` and `psp_fired`, which are
/// what `A_WeaponReady` and `P_FireWeapon` do to the player's own mobj.
pub fn move_psprites(
    state: &State,
    bob: &str,
    buttons: &str,
    pendingweapon: &str,
) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let w = |field: usize| format!("psp_at.{field}");
    let i = "psp_step.1";
    let entry = "psp_step.2";
    let at = |field: usize| format!("{}[{i}]", w(field));

    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    // The state this step enters: the one the tic count ran out on for the
    // first entry, and whatever the entry before it redirected to after.
    value(
        "psp_entering",
        format!(
            "toInt32(if({entry} = 0, state_nextstate[1 + {}], {}))",
            at(held::STATE),
            at(held::PENDING)
        ),
    );
    value(
        "psp_runs",
        format!(
            "toUInt8({entry} = 0 OR {} != {NO_STATE})",
            at(held::PENDING)
        ),
    );
    // `P_SetPsprite` stores a null pointer for `S_NULL` and stops.
    value(
        "psp_removes",
        "toUInt8(psp_runs = 1 AND psp_entering = 0)".to_owned(),
    );
    value(
        "psp_enters",
        "toUInt8(psp_runs = 1 AND psp_entering != 0)".to_owned(),
    );
    value(
        "psp_action",
        "toInt32(if(psp_enters = 1, state_action[1 + psp_entering], 0))".to_owned(),
    );
    value(
        "psp_tics_entered",
        "toInt32(if(psp_enters = 1, state_tics[1 + psp_entering], 0))".to_owned(),
    );

    // `A_Raise`, `A_Lower` and `A_WeaponReady`, each reading the position
    // the entry above left and answering with the position it leaves, the
    // state it redirects to, and whether it could be run at all.
    value(
        "psp_raised",
        format!("toInt64({} - {RAISESPEED})", at(held::SY)),
    );
    value(
        "psp_lowered",
        format!("toInt64({} + {LOWERSPEED})", at(held::SY)),
    );
    // `P_BringUpWeapon` brings up whatever `A_Lower` put in hand, and
    // takes `readyweapon` when nothing is pending.
    value(
        "psp_brought_up",
        format!(
            "toInt64(if({p} = {WP_NOCHANGE}, {r}, {p}))",
            p = w(held::PENDINGWEAPON),
            r = w(held::READYWEAPON)
        ),
    );
    value(
        "psp_ready_bob_angle",
        format!(
            "toInt64(bitAnd(128 * toInt64({}), {FINEMASK}))",
            s("leveltime")
        ),
    );
    value(
        "psp_ready_sx",
        format!(
            "toInt64({FRACUNIT} + toInt64({}))",
            fixed::fixed_mul(bob, &maputl::finecosine("psp_ready_bob_angle"))
        ),
    );
    value(
        "psp_ready_sy",
        format!(
            "toInt64({WEAPONTOP} + toInt64({}))",
            fixed::fixed_mul(
                bob,
                &maputl::finesine(&format!("bitAnd(psp_ready_bob_angle, {FINEANGLES_HALF})"))
            )
        ),
    );
    // `A_WeaponReady` puts the weapon away when one is pending and fires
    // it when the attack button is down.
    value(
        "psp_ready_changes",
        format!(
            "toUInt8(psp_action = a_weaponready AND ({} != {WP_NOCHANGE} OR {} = 0))",
            w(held::PENDINGWEAPON),
            s("p_health")
        ),
    );
    value(
        "psp_attack_held",
        format!("toUInt8(bitAnd(toInt64({buttons}), {BT_ATTACK}) != 0)"),
    );
    // The launcher and the BFG want the button let go of between shots.
    value(
        "psp_ready_fires",
        format!(
            "toUInt8(psp_action = a_weaponready AND psp_ready_changes = 0 \
             AND psp_attack_held = 1 \
             AND ({down} = 0 OR ({r} != {WP_MISSILE} AND {r} != {WP_BFG})))",
            down = w(held::ATTACKDOWN),
            r = w(held::READYWEAPON)
        ),
    );
    // `P_CheckAmmo`. A weapon with nothing left picks another one, which
    // this does not write.
    value(
        "psp_shot_costs",
        format!(
            "toInt32(multiIf({r} = {WP_BFG}, {BFGCELLS}, {r} = {WP_SUPERSHOTGUN}, 2, 1))",
            r = w(held::READYWEAPON)
        ),
    );
    value(
        "psp_has_ammo",
        format!(
            "toUInt8(weapon_ammo[1 + {r}] = {AM_NOAMMO} \
             OR {ammo}[1 + weapon_ammo[1 + {r}]] >= psp_shot_costs)",
            r = w(held::READYWEAPON),
            ammo = s("p_ammo")
        ),
    );
    value(
        "psp_fires",
        "toUInt8(psp_ready_fires = 1 AND psp_has_ammo = 1)".to_owned(),
    );
    // `A_FireShotgun` sends seven shots down the barrel and `A_FirePistol`
    // one, which goes where the weapon points unless the trigger was held.
    // Both spend a round and put the flash sprite in the weapon's own
    // flash frame.
    value(
        "psp_fires_shots",
        "toUInt8(psp_action = a_fireshotgun OR psp_action = a_firepistol)".to_owned(),
    );
    value(
        "psp_shot_count",
        "toUInt32(multiIf(psp_action = a_fireshotgun, 7, psp_action = a_firepistol, 1, 0))"
            .to_owned(),
    );
    value(
        "psp_shot_accurate",
        format!(
            "toUInt8(psp_action = a_firepistol AND {} = 0)",
            s("p_refire")
        ),
    );
    value(
        "psp_flash_entered",
        format!(
            "toInt32(if(psp_fires_shots = 1, weapon_flashstate[1 + {r}], {NO_STATE}))",
            r = w(held::READYWEAPON)
        ),
    );
    value(
        "psp_lower_finishes",
        format!("toUInt8(psp_action = a_lower AND psp_lowered >= {WEAPONBOTTOM})"),
    );
    value(
        "psp_raise_finishes",
        format!("toUInt8(psp_action = a_raise AND psp_raised <= {WEAPONTOP})"),
    );
    // Where the entry leaves the sprite.
    value(
        "psp_sx_now",
        format!(
            "toInt32(if(psp_action = a_weaponready AND psp_ready_changes = 0 \
             AND psp_ready_fires = 0, psp_ready_sx, toInt64({})))",
            at(held::SX)
        ),
    );
    value(
        "psp_sy_now",
        format!(
            "toInt32(multiIf(\
             psp_action = a_raise AND psp_raise_finishes = 1, {WEAPONTOP}, \
             psp_action = a_raise, psp_raised, \
             psp_lower_finishes = 1, {WEAPONBOTTOM}, \
             psp_action = a_lower, psp_lowered, \
             psp_action = a_weaponready AND psp_ready_changes = 0 \
             AND psp_ready_fires = 0, psp_ready_sy, \
             toInt64({})))",
            at(held::SY)
        ),
    );
    // The state the routine redirects to, or -1 when it returns.
    value(
        "psp_redirect",
        format!(
            "toInt32(multiIf(\
             psp_raise_finishes = 1, weapon_readystate[1 + {r}], \
             psp_ready_changes = 1, weapon_downstate[1 + {r}], \
             psp_lower_finishes = 1, weapon_upstate[1 + psp_brought_up], \
             psp_fires = 1, weapon_atkstate[1 + {r}], \
             {NO_STATE}))",
            r = w(held::READYWEAPON)
        ),
    );
    // A routine this does not run, and the branches of the ones it does
    // that it cannot answer for.
    let flash_held = format!("{}[{PS_FLASH}]", s("psp_state"));
    value(
        "psp_stuck",
        format!(
            "toUInt8(psp_enters = 1 AND (\
             (psp_action != 0 AND psp_action != a_weaponready \
             AND psp_action != a_lower AND psp_action != a_raise \
             AND psp_action != a_light0 AND psp_action != a_light1 \
             AND psp_action != a_light2 AND psp_fires_shots = 0) \
             OR (psp_ready_fires = 1 AND psp_has_ammo = 0) \
             OR (psp_fires_shots = 1 AND (psp_flash_entered = 0 \
             OR (state_action[1 + psp_flash_entered] != a_light0 \
             AND state_action[1 + psp_flash_entered] != a_light1 \
             AND state_action[1 + psp_flash_entered] != a_light2) \
             OR state_tics[1 + psp_flash_entered] - 1 = 0 \
             OR {} != {NO_STATE})) \
             OR (psp_action = a_lower AND psp_lowered >= {WEAPONBOTTOM} \
             AND ({} = {PST_DEAD} OR {} = 0))))",
            flash_held,
            s("p_playerstate"),
            s("p_health")
        ),
    );

    let put = |array: String, value: &str| {
        format!("arrayMap((v, k) -> if(k = {i}, {value}, v), {array}, arrayEnumerate({array}))")
    };
    // The accumulator's members, in the order `held` names them.
    let state_now = format!("toInt32(if(psp_removes = 1, {NO_STATE}, psp_entering))");
    let tics_now = format!(
        "toInt32(if(psp_removes = 1, {}, psp_tics_entered))",
        at(held::TICS)
    );
    // `A_Lower` puts the weapon it lowered off the screen in hand, and
    // `P_BringUpWeapon` clears the ask once it has brought one up.
    let readyweapon_now = format!(
        "toInt32(if(psp_lower_finishes = 1, psp_brought_up, {}))",
        w(held::READYWEAPON)
    );
    let pendingweapon_now = format!(
        "toInt32(if(psp_lower_finishes = 1, {WP_NOCHANGE}, {}))",
        w(held::PENDINGWEAPON)
    );
    // `A_WeaponReady` marks the button down before it fires, and lets it up
    // only where it is up. A held launcher leaves it as it stands.
    let attackdown_now = format!(
        "toUInt8(multiIf(psp_ready_fires = 1, 1, \
         psp_action = a_weaponready AND psp_ready_changes = 0 AND psp_attack_held = 0, 0, {}))",
        w(held::ATTACKDOWN)
    );
    // The cycle carries on while the routine redirected or the state it
    // entered waits no tics.
    let pending_now = format!(
        "toInt32(multiIf(psp_removes = 1, {NO_STATE}, \
         psp_redirect != {NO_STATE}, psp_redirect, \
         psp_tics_entered = 0, state_nextstate[1 + psp_entering], {NO_STATE}))"
    );
    let unresolved_now = format!("toUInt8({} = 1 OR psp_stuck = 1)", w(held::UNRESOLVED));
    let readied_now = format!(
        "toUInt8({} = 1 OR psp_action = a_weaponready)",
        w(held::READIED)
    );
    let fired_now = format!("toUInt8({} = 1 OR psp_fires = 1)", w(held::FIRED));
    // One entry of one tic fires, so the count, the spread and the flash
    // frame the entry that did carry through to the end of the fold.
    let shots_now = format!(
        "toUInt32(if(psp_fires_shots = 1, psp_shot_count, {}))",
        w(held::SHOTS)
    );
    let accurate_now = format!(
        "toUInt8(if(psp_fires_shots = 1, psp_shot_accurate, {}))",
        w(held::ACCURATE)
    );
    let flash_now = format!(
        "toInt32(if(psp_fires_shots = 1, psp_flash_entered, {}))",
        w(held::FLASH)
    );
    // `A_Light0`, `A_Light1` and `A_Light2` are the whole of what a
    // psprite routine leaves outside the sprite itself. An entry that
    // fires runs the flash frame's own routine as it puts the sprite
    // there, and a flash frame the sprite cycles into runs it as any
    // state does.
    let light = |state: &str, held: &str| {
        format!(
            "multiIf(state_action[1 + {state}] = a_light1, toInt32(1), \
             state_action[1 + {state}] = a_light2, toInt32(2), \
             state_action[1 + {state}] = a_light0, toInt32(0), {held})"
        )
    };
    let extralight_now = format!(
        "toInt32(if(psp_fires_shots = 1, {}, {}))",
        light("psp_flash_entered", &w(held::EXTRALIGHT)),
        light("greatest(psp_entering, 0)", &w(held::EXTRALIGHT)),
    );
    let members = [
        put(w(held::STATE), &state_now),
        put(w(held::TICS), &tics_now),
        put(w(held::SX), "psp_sx_now"),
        put(w(held::SY), "psp_sy_now"),
        readyweapon_now,
        pendingweapon_now,
        attackdown_now,
        put(w(held::PENDING), &pending_now),
        unresolved_now,
        readied_now,
        fired_now,
        shots_now,
        accurate_now,
        flash_now,
        extralight_now,
    ];
    let body = format!("if(psp_runs = 0, psp_at, ({}))", members.join(", "));

    // `P_MovePsprites` drops the count before it asks whether the state
    // changes, and a count of -1 never changes.
    let dropped = format!(
        "arrayMap((t, k) -> toInt32(if({st}[k] != {NO_STATE} AND t != -1, t - 1, t)), {t}, \
         arrayEnumerate({t}))",
        st = s("psp_state"),
        t = s("psp_tics")
    );
    let start = format!(
        "({}, {dropped}, {}, {}, toInt32({}), toInt32({pendingweapon}), toUInt8({}), \
         CAST([{NO_STATE}, {NO_STATE}], 'Array(Int32)'), toUInt8(0), toUInt8(0), toUInt8(0), \
         toUInt32(0), toUInt8(0), toInt32({NO_STATE}), toInt32({}))",
        s("psp_state"),
        s("psp_sx"),
        s("psp_sy"),
        s("p_readyweapon"),
        s("p_attackdown"),
        s("p_extralight"),
    );
    // One step per entry each cycling sprite is given, in sprite order.
    // A sprite whose count did not run out contributes none, so a tic that
    // moves neither walks nothing.
    let cycling = format!(
        "arrayFilter(k -> {st}[k] != {NO_STATE} AND {t}[k] != -1 AND {t}[k] - 1 = 0, \
         arrayEnumerate({st}))",
        st = s("psp_state"),
        t = s("psp_tics"),
    );
    let work =
        format!("arrayFlatten(arrayMap(k -> arrayMap(e -> (k, e), range({ENTRIES})), {cycling}))");

    let ran = format!(
        "arrayFold((psp_at, psp_step) -> {}, {work}, {start})",
        bind::chain_in("psp", &values, &body)
    );
    let held = |field: usize| format!("psprites.{field}");
    vec![
        ("psprites".to_owned(), ran),
        (
            "psp_shots".to_owned(),
            format!("toUInt32({})", held(held::SHOTS)),
        ),
        (
            "psp_accurate".to_owned(),
            format!("toUInt8({})", held(held::ACCURATE)),
        ),
        (
            "psp_flash".to_owned(),
            format!("toInt32({})", held(held::FLASH)),
        ),
        // `P_SetPsprite` puts the flash sprite in the weapon's flash frame
        // as the weapon's own entry runs, and `P_MovePsprites` reaches the
        // flash after the weapon, so the frame it was just put in is a tic
        // shorter by the time the tic ends.
        (
            "now_psp_state".to_owned(),
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {PS_FLASH} AND psp_flash != {NO_STATE}, \
                 psp_flash, v)), {s}, arrayEnumerate({s}))",
                s = held(held::STATE)
            ),
        ),
        (
            "now_psp_tics".to_owned(),
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {PS_FLASH} AND psp_flash != {NO_STATE}, \
                 state_tics[1 + psp_flash] - 1, v)), {t}, arrayEnumerate({t}))",
                t = held(held::TICS)
            ),
        ),
        // `P_MovePsprites` ends by putting the flash sprite where the
        // weapon sprite is, so both elements take the weapon's.
        (
            "now_psp_sx".to_owned(),
            format!(
                "CAST([{s}[{PS_WEAPON}], {s}[{PS_WEAPON}]], 'Array(Int32)')",
                s = held(held::SX)
            ),
        ),
        (
            "now_psp_sy".to_owned(),
            format!(
                "CAST([{s}[{PS_WEAPON}], {s}[{PS_WEAPON}]], 'Array(Int32)')",
                s = held(held::SY)
            ),
        ),
        (
            "now_p_readyweapon".to_owned(),
            format!("toInt32({})", held(held::READYWEAPON)),
        ),
        (
            "psp_pendingweapon".to_owned(),
            format!("toInt32({})", held(held::PENDINGWEAPON)),
        ),
        (
            "now_p_attackdown".to_owned(),
            format!("toUInt8({})", held(held::ATTACKDOWN)),
        ),
        (
            "psp_unresolved".to_owned(),
            format!(
                "toUInt8({} = 1 OR arrayExists(k -> {p}[k] != {NO_STATE}, arrayEnumerate({p})))",
                held(held::UNRESOLVED),
                p = held(held::PENDING)
            ),
        ),
        (
            "psp_readied".to_owned(),
            format!("toUInt8({})", held(held::READIED)),
        ),
        (
            "psp_fired".to_owned(),
            format!("toUInt8({})", held(held::FIRED)),
        ),
        (
            "now_p_extralight".to_owned(),
            format!("toInt32({})", held(held::EXTRALIGHT)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables;

    fn bindings() -> Vec<(String, String)> {
        move_psprites(
            &State::default(),
            "now_p_bob",
            "pl_buttons",
            "pl_pendingweapon",
        )
    }

    fn fold() -> String {
        bindings()
            .into_iter()
            .find(|(name, _)| name == "psprites")
            .map(|(_, expr)| expr)
            .expect("the cycle is one binding")
    }

    /// `P_SetPsprite` copies `misc1` and `misc2` into the sprite's
    /// position, and no state the engine's own table carries sets either,
    /// so the branch is not written. This fails if one ever does.
    #[test]
    fn no_state_sets_the_sprite_position() {
        let states = tables::table("states").expect("the states table is committed");
        for column in ["misc1", "misc2"] {
            let values = states.ints(column).expect("the column is an integer");
            assert!(
                values.iter().all(|value| *value == 0),
                "a state sets {column}, so P_SetPsprite's coordinate branch is reachable"
            );
        }
    }

    /// The list holds a step per entry per cycling sprite, so a tic where
    /// neither count runs out walks nothing and the body is skipped.
    #[test]
    fn the_cycle_is_one_fold_over_the_sprites_that_move() {
        let sql = fold();
        assert_eq!(sql.matches("arrayFold((psp_at, psp_step)").count(), 1);
        assert!(sql.contains(&format!("range({ENTRIES})")), "{sql}");
        assert!(
            sql.contains("arrayFilter(k -> prev_psp_state[k] != -1"),
            "{sql}"
        );
    }

    /// A routine this does not run leaves the tic unresolved rather than
    /// putting the sprite somewhere the engine did not.
    ///
    /// The dispatch compares one routine at a time. `IN` over a set whose
    /// members are not literals answers `Nullable(UInt8)`, and the fold's
    /// accumulator has no nullable member.
    #[test]
    fn an_unwritten_routine_leaves_the_tic_unresolved() {
        let sql = fold();
        for routine in ["a_weaponready", "a_lower", "a_raise"] {
            assert!(sql.contains(&format!("!= {routine}")), "{routine}: {sql}");
        }
        assert!(!sql.contains(" IN ("), "{sql}");
    }

    #[test]
    fn every_binding_balances_its_parentheses() {
        for (name, expr) in bindings() {
            let depth = expr.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{name}");
        }
    }
}

// ---------------------------------------------------------------------------
// The shots
// ---------------------------------------------------------------------------

/// `p_local.h`: how far a hitscan reaches.
const MISSILERANGE: i64 = 32 * 64 * FRACUNIT;
/// `p_pspr.c`: how far a shot that is not aimed spreads.
const SPREADSHIFT: u32 = 18;
/// `p_mobj.h`
const MF_NOBLOOD: i64 = 0x8_0000;
/// `tables.h`
const ANGLE_WRAP: i64 = 1 << 32;

/// Where each field of the shot fold's accumulator sits.
mod firing {
    pub const HEALTH: usize = 1;
    pub const FLAGS: usize = 2;
    pub const STATE: usize = 3;
    pub const TICS: usize = 4;
    pub const MOMX: usize = 5;
    pub const MOMY: usize = 6;
    pub const MOMZ: usize = 7;
    pub const HEIGHT: usize = 8;
    pub const TARGET: usize = 9;
    pub const THRESHOLD: usize = 10;
    pub const REACTIONTIME: usize = 11;
    /// The things the shots have spawned, in the order they were spawned.
    pub const SPAWNED: usize = 12;
    /// How many numbers the shots have drawn.
    pub const DRAWS: usize = 13;
    /// How many of the deaths count towards the kill total.
    pub const KILLS: usize = 14;
    pub const STUCK: usize = 15;
    /// The slope `P_BulletSlope` found, which every shot leaves at.
    pub const SLOPE: usize = 16;
}

/// `P_GunShot` for each shot the weapon's own routine sent, in order.
///
/// The shots are a fold rather than a map because how many numbers a shot
/// draws depends on what the shot before it reached: three for the shot,
/// four more for the puff or the blood spot it leaves, and one or two more
/// where it damages what it hit. A tic that fires nothing folds over an
/// empty list and pays for the list alone.
///
/// The spawned things are carried rather than added to the list, because a
/// puff and a blood spot carry `MF_NOBLOCKMAP` and no later shot can reach
/// them. The writeback puts them on the end.
pub fn fire_shots(state: &State) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let at = |field: usize| format!("gs_at.{field}");
    let mut bindings: Vec<(String, String)> = Vec::new();
    let mut bind = |name: &str, expr: String| bindings.push((name.to_owned(), expr));

    // Where every mobj stands when the weapon fires. Nothing a shot does
    // moves one, so these are read once for the whole fold; what a shot
    // does change travels in the accumulator.
    let prnd = s("prndindex");
    for column in [
        "m_x",
        "m_y",
        "m_z",
        "m_radius",
        "m_linkseq",
        "m_type",
        "m_player",
        "sec_floorheight",
        "sec_ceilingheight",
        "line_special",
    ] {
        bind(&format!("gs_{column}"), s(column));
    }
    bind(
        "gs_alive",
        format!("arrayMap(v -> toUInt8(1), {})", s("m_x")),
    );
    // The accumulator's own arrays, named once so the damage call and the
    // trace can both read what the shots before them left.
    let held: Vec<String> = (1..=firing::REACTIONTIME).map(at).collect();
    let member = |field: usize| held[field - 1].as_str();

    // One shot: its own draws, where it ends, what it leaves there and
    // what it does to what it hit.
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({}) + {nth}, 255)])",
            s("prndindex"),
            at(firing::DRAWS),
        )
    };
    value("gs_aiming", "toUInt8(gs_i = 0)".to_owned());
    value("gs_damage", format!("toInt32(5 * ({} % 3 + 1))", draw("1")));
    // An inaccurate shot spreads either side of where the weapon points.
    value(
        "gs_angle",
        format!(
            "toUInt32(bitAnd(toUInt64(pl_new_angle) + if(psp_accurate = 1, 0, \
             toUInt64(bitAnd(bitShiftLeft({} - {}, {SPREADSHIFT}), {}))), {}))",
            draw("2"),
            draw("3"),
            ANGLE_WRAP - 1,
            ANGLE_WRAP - 1,
        ),
    );
    value(
        "gs_shot_draws",
        "toUInt32(multiIf(gs_aiming = 1, 0, psp_accurate = 1, 1, 3))".to_owned(),
    );
    // One walk serves the aim and the shots. Step 0 asks the three angles
    // `P_BulletSlope` tries and every step after it asks the one the shot
    // leaves at, so the trace, its intercepts and their order are in the
    // statement once.
    let aim = |by: i64| {
        shoot::asking(
            "pl_slot",
            "pl_x",
            "pl_y",
            "pl_z",
            "pl_height",
            &format!(
                "toUInt32(bitAnd(toUInt64(pl_new_angle) + {}, {}))",
                by.rem_euclid(ANGLE_WRAP),
                ANGLE_WRAP - 1
            ),
            &shoot::AIMRANGE.to_string(),
        )
    };
    value(
        "gs_asks",
        format!(
            "if(gs_aiming = 1, [{}, {}, {}], [{}])",
            aim(0),
            aim(shoot::AIMSWING),
            aim(-shoot::AIMSWING),
            shoot::shooting(
                "pl_slot",
                "pl_x",
                "pl_y",
                "pl_z",
                "pl_height",
                "gs_angle",
                &MISSILERANGE.to_string(),
                &at(firing::SLOPE),
            )
        ),
    );
    value(
        "gs_answers",
        shoot::traverse(
            "gs_asks",
            &targets(member(firing::FLAGS), member(firing::HEIGHT)),
        ),
    );
    // `P_BulletSlope` stops at the first try that found a target, and a
    // try that found none answers 0 and moves nothing.
    value(
        "gs_slope_now",
        format!(
            "toInt32(if(gs_aiming = 0, {held}, \
             arrayFirst(v -> 1, arrayPushBack(arrayMap(a -> toInt32(a.{slope}), \
             arrayFilter(a -> a.{target} != 0, gs_answers)), \
             toInt32(gs_answers[-1].{slope})))))",
            held = at(firing::SLOPE),
            slope = shoot::reached::SLOPE,
            target = shoot::reached::TARGET,
        ),
    );
    value("gs_reached", "gs_answers[1]".to_owned());
    value(
        "gs_kind",
        format!(
            "toUInt8(if(gs_aiming = 1, 0, gs_reached.{}))",
            shoot::reached::KIND
        ),
    );
    value(
        "gs_id",
        format!("toInt32(gs_reached.{})", shoot::reached::ID),
    );
    // `P_SpawnPuff` for a wall and for a thing that cannot bleed,
    // `P_SpawnBlood` for one that can.
    value(
        "gs_blood",
        format!(
            "toUInt8(gs_kind = 2 AND bitAnd({}[gs_id], {MF_NOBLOOD}) = 0)",
            at(firing::FLAGS)
        ),
    );
    value(
        "gs_born",
        mobj::spawn_debris(
            &format!(
                "arraySlice([(gs_blood, toInt32(gs_reached.{}), toInt32(gs_reached.{}), \
                 toInt32(gs_reached.{}), gs_damage, toInt32({MISSILERANGE}), \
                 toUInt32({} + gs_shot_draws))], 1, toUInt8(gs_kind != 0))",
                shoot::reached::X,
                shoot::reached::Y,
                shoot::reached::Z,
                at(firing::DRAWS),
            ),
            &mobj::Spawning {
                floorheight: "gs_sec_floorheight",
                ceilingheight: "gs_sec_ceilingheight",
                prndindex: &s("prndindex"),
                skill: "skill",
            },
        ),
    );
    value(
        "gs_spawn_draws",
        "toUInt32(if(gs_kind = 0, 0, 4))".to_owned(),
    );
    // `P_DamageMobj` runs where the shot ended on a thing, and a hitscan's
    // source is its own inflictor.
    value(
        "gs_hurt",
        inter::damage_mobj(
            &format!(
                "arraySlice([(toUInt32(gs_id), toUInt32(pl_slot), toUInt32(pl_slot), gs_damage, \
                 toUInt32({} + gs_shot_draws + gs_spawn_draws))], 1, toUInt8(gs_kind = 2))",
                at(firing::DRAWS),
            ),
            &inter::Hurting {
                m_x: "gs_m_x",
                m_y: "gs_m_y",
                m_z: "gs_m_z",
                m_momx: member(firing::MOMX),
                m_momy: member(firing::MOMY),
                m_momz: member(firing::MOMZ),
                m_reactiontime: member(firing::REACTIONTIME),
                m_type: "gs_m_type",
                m_state: member(firing::STATE),
                m_tics: member(firing::TICS),
                m_flags: member(firing::FLAGS),
                m_health: member(firing::HEALTH),
                m_height: member(firing::HEIGHT),
                m_target: member(firing::TARGET),
                m_threshold: member(firing::THRESHOLD),
                m_player: "gs_m_player",
                prndindex: &prnd,
                readyweapon: "now_p_readyweapon",
            },
        ),
    );
    value(
        "gs_hit",
        format!(
            "arrayFirst(v -> 1, arrayPushBack(gs_hurt, {none}))",
            none = inter::no_hurt()
        ),
    );
    value(
        "gs_draws_now",
        format!(
            "toUInt32({} + gs_shot_draws + gs_spawn_draws + toUInt32(gs_hit.{}))",
            at(firing::DRAWS),
            inter::hurt::DRAWS,
        ),
    );
    // A special line the shot crossed is `P_ShootSpecialLine`, which this
    // does not run.
    value(
        "gs_stuck_now",
        format!(
            "toUInt8({} = 1 OR gs_hit.{} = 1 OR notEmpty(gs_reached.{}))",
            at(firing::STUCK),
            inter::hurt::STUCK,
            shoot::reached::SPECHIT,
        ),
    );

    let hurt_into = |column: usize, member: usize, cast: &str| {
        format!(
            "arrayMap((v, k) -> {cast}(if(gs_kind = 2 AND k = gs_id, gs_hit.{member}, v)), \
             {a}, arrayEnumerate({a}))",
            a = at(column)
        )
    };
    let members = [
        hurt_into(firing::HEALTH, inter::hurt::HEALTH, "toInt32"),
        hurt_into(firing::FLAGS, inter::hurt::FLAGS, "toInt32"),
        hurt_into(firing::STATE, inter::hurt::STATE, "toInt32"),
        hurt_into(firing::TICS, inter::hurt::TICS, "toInt32"),
        hurt_into(firing::MOMX, inter::hurt::MOMX, "toInt32"),
        hurt_into(firing::MOMY, inter::hurt::MOMY, "toInt32"),
        hurt_into(firing::MOMZ, inter::hurt::MOMZ, "toInt32"),
        hurt_into(firing::HEIGHT, inter::hurt::HEIGHT, "toInt32"),
        hurt_into(firing::TARGET, inter::hurt::TARGET, "toUInt32"),
        hurt_into(firing::THRESHOLD, inter::hurt::THRESHOLD, "toInt32"),
        hurt_into(firing::REACTIONTIME, inter::hurt::REACTIONTIME, "toInt32"),
        format!("arrayConcat({}, gs_born)", at(firing::SPAWNED)),
        "gs_draws_now".to_owned(),
        format!(
            "toInt32({} + toInt32(gs_hit.{}))",
            at(firing::KILLS),
            inter::hurt::COUNTED
        ),
        "gs_stuck_now".to_owned(),
        "gs_slope_now".to_owned(),
    ];
    let empty_spawns = format!("CAST([], 'Array({})')", mobj::BORN_TYPE);
    let start = format!(
        "({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, toUInt32(0), toInt32(0), toUInt8(0), \
         toInt32(0))",
        s("m_health"),
        s("m_flags"),
        s("m_state"),
        s("m_tics"),
        s("m_momx"),
        s("m_momy"),
        s("m_momz"),
        s("m_height"),
        s("m_target"),
        s("m_threshold"),
        s("m_reactiontime"),
        empty_spawns,
    );
    bind(
        "gs_ran",
        format!(
            "arrayFold((gs_at, gs_i) -> {}, \
             range(toUInt32(if(psp_shots > 0, psp_shots + 1, 0))), {start})",
            bind::chain_in("gsa", &values, &format!("({})", members.join(", ")))
        ),
    );

    // What the shots left. The mobj columns the damage moves go back as
    // they stand; the things the shots spawned wait for the writeback,
    // which is what puts them on the end of the list.
    let ran = |field: usize| format!("gs_ran.{field}");
    for (column, field, cast) in [
        ("m_health", firing::HEALTH, "toInt32"),
        ("m_flags", firing::FLAGS, "toInt32"),
        ("m_state", firing::STATE, "toInt32"),
        ("m_tics", firing::TICS, "toInt32"),
        ("m_momx", firing::MOMX, "toInt32"),
        ("m_momy", firing::MOMY, "toInt32"),
        ("m_momz", firing::MOMZ, "toInt32"),
        ("m_height", firing::HEIGHT, "toInt32"),
        ("m_target", firing::TARGET, "toUInt32"),
        ("m_threshold", firing::THRESHOLD, "toInt32"),
        ("m_reactiontime", firing::REACTIONTIME, "toInt32"),
    ] {
        bind(
            &format!("gs_{column}"),
            format!("arrayMap(v -> {cast}(v), {})", ran(field)),
        );
    }
    bind("gs_spawned", ran(firing::SPAWNED));
    bind(
        "now_prndindex",
        format!(
            "toUInt8(bitAnd(toUInt32({}) + {}, 255))",
            s("prndindex"),
            ran(firing::DRAWS)
        ),
    );
    // `DecreaseAmmo` takes one round of whatever the weapon in hand eats.
    bind(
        "gs_p_ammo",
        format!(
            "arrayMap((v, k) -> toInt32(if(psp_shots > 0 \
             AND k = 1 + weapon_ammo[1 + now_p_readyweapon], v - 1, v)), {a}, arrayEnumerate({a}))",
            a = s("p_ammo")
        ),
    );
    bind(
        "now_p_killcount",
        format!("toInt32({} + {})", s("p_killcount"), ran(firing::KILLS)),
    );
    bind("gs_unresolved", format!("toUInt8({})", ran(firing::STUCK)));
    bindings
}

/// The arrays a shot's trace reads. Where a thing stands does not move
/// while the shots run; what it carries and how tall it is do, so those
/// two come from wherever the caller is holding them.
fn targets<'a>(flags: &'a str, height: &'a str) -> shoot::Targets<'a> {
    shoot::Targets {
        m_x: "gs_m_x",
        m_y: "gs_m_y",
        m_z: "gs_m_z",
        m_radius: "gs_m_radius",
        m_height: height,
        m_flags: flags,
        m_linkseq: "gs_m_linkseq",
        alive: "gs_alive",
        floorheight: "gs_sec_floorheight",
        ceilingheight: "gs_sec_ceilingheight",
        line_special: "gs_line_special",
    }
}
