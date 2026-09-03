//! The player's weapon sprites, from `p_pspr.c`.
//!
//! `P_MovePsprites` runs the two sprites' state cycles inside
//! `P_PlayerThink`, and `P_SetPsprite` may enter several states in one
//! call because an action routine can put the sprite somewhere else. Both
//! are one fold: its list holds a step for each state entry each cycling
//! sprite is given, so a tic where neither cycles walks nothing.

use crate::sql::bind;
use crate::sql::fixed;

use super::{State, maputl};

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
/// `d_player.h`: `PST_DEAD`.
const PST_DEAD: i64 = 2;
/// `p_pspr.h`: the weapon sprite, one-based for the arrays that hold both.
const PS_WEAPON: usize = 1;
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
        ("a_weaponready".to_owned(), action("A_WeaponReady")),
        ("a_lower".to_owned(), action("A_Lower")),
        ("a_raise".to_owned(), action("A_Raise")),
    ]
}

/// `P_MovePsprites`: the two sprites' cycles, then the flash sprite taking
/// the weapon's position.
///
/// `bob` is `player->bob` as `P_CalcHeight` left it, `buttons` the tic
/// command's, and `pendingweapon` the weapon the command asked for. The
/// stage names `now_p_readyweapon` and `psp_pendingweapon`, which the
/// pickups after it read.
pub fn move_psprites(
    state: &State,
    bob: &str,
    buttons: &str,
    pendingweapon: &str,
    mobj_state: &str,
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
        "psp_ready_fires",
        format!(
            "toUInt8(psp_action = a_weaponready AND psp_ready_changes = 0 \
             AND bitAnd(toInt64({buttons}), {BT_ATTACK}) != 0)"
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
             {NO_STATE}))",
            r = w(held::READYWEAPON)
        ),
    );
    // A routine this does not run, and the branches of the ones it does
    // that it cannot answer for.
    value(
        "psp_stuck",
        format!(
            "toUInt8(psp_enters = 1 AND (\
             (psp_action != 0 AND psp_action != a_weaponready \
             AND psp_action != a_lower AND psp_action != a_raise) \
             OR psp_ready_fires = 1 \
             OR (psp_action = a_weaponready \
             AND ({mobj_state} = s_play_atk1 \
             OR {mobj_state} = state_nextstate[1 + s_play_atk1])) \
             OR (psp_action = a_lower AND psp_lowered >= {WEAPONBOTTOM} \
             AND ({} = {PST_DEAD} OR {} = 0))))",
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
    let attackdown_now = format!(
        "toUInt8(multiIf(psp_ready_fires = 1, 1, \
         psp_action = a_weaponready AND psp_ready_changes = 0, 0, {}))",
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
         CAST([{NO_STATE}, {NO_STATE}], 'Array(Int32)'), toUInt8(0))",
        s("psp_state"),
        s("psp_sx"),
        s("psp_sy"),
        s("p_readyweapon"),
        s("p_attackdown"),
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
        ("now_psp_state".to_owned(), held(held::STATE)),
        ("now_psp_tics".to_owned(), held(held::TICS)),
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
            "pl_state_moved",
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
