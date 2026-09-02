//! Where a tic's command comes from, from `g_game.c`.
//!
//! `G_BuildTiccmd` runs every tic and builds a command from the keys and
//! the mouse. When a demo is playing, `G_ReadDemoTiccmd` then overwrites
//! the four fields the lump carries, and what the keys built survives only
//! in `turnheld`.

use clickdoom_spec::native_state::key;

use super::State;

/// `g_game.c`: how far one step of each kind carries, walking then running.
const FORWARDMOVE: [i32; 2] = [0x19, 0x32];
const SIDEMOVE: [i32; 2] = [0x18, 0x28];
/// `g_game.c`: the turn per tic, walking, running, then the slow first
/// turns a tap makes.
const ANGLETURN: [i32; 3] = [640, 1280, 320];
/// `g_game.c`: how long a turn key is held before it turns at full speed.
const SLOWTURNTICS: i32 = 6;
/// `g_game.c`: the furthest one command can carry.
const MAXPLMOVE: i32 = FORWARDMOVE[1];
/// `d_loop.c`: how many tics one sampled command covers.
const TICDUP: i32 = 1;

/// `d_event.h`
const BT_ATTACK: i32 = 1;
const BT_USE: i32 = 2;
const BT_SPECIAL: i32 = 128;
const BT_SPECIALMASK: i32 = 3;
const BT_CHANGE: i32 = 4;
const BT_WEAPONSHIFT: i32 = 3;
const BTS_PAUSE: i32 = 1;

/// How many weapon keys the key word carries.
const WEAPON_KEYS: u32 = 7;

/// The tic command, and the turn the keys have built up.
///
/// The demo's commands enter as a constant array indexed by tic, because
/// the lump is fixed for a session and a tic reads one entry of it.
pub fn command(state: &State, db: &str) -> Vec<(String, String)> {
    let mut bindings = demo(db);
    bindings.extend(build_ticcmd(state));
    let playing = "source = 0 AND tic <= length(demo_forwardmove)";
    let field = |name: &str, cast: &str, built: &str| {
        (
            format!("now_p_cmd_{name}"),
            format!("{cast}(if({playing}, demo_{name}[tic], {built}))"),
        )
    };
    bindings.extend([
        field("forwardmove", "toInt8", "cmd_forwardmove"),
        field("sidemove", "toInt8", "cmd_sidemove"),
        field("angleturn", "toInt16", "cmd_angleturn"),
        field("buttons", "toUInt8", "cmd_buttons"),
        (
            "now_demo_end".to_owned(),
            "toUInt8(source = 0 AND tic > length(demo_forwardmove))".to_owned(),
        ),
    ]);
    bindings
}

/// `G_ReadDemoTiccmd`'s source: the lump's four bytes per tic.
fn demo(db: &str) -> Vec<(String, String)> {
    let column = |name: &str| {
        (
            format!("demo_{name}"),
            format!(
                "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((tic, {name}))))\
                 \n     FROM {db}.demo_cmds)"
            ),
        )
    };
    ["forwardmove", "sidemove", "angleturn", "buttons"]
        .into_iter()
        .map(column)
        .collect()
}

/// `G_BuildTiccmd` over the key word and the mouse deltas a session
/// streams.
///
/// The joystick, the mouse buttons and the double-click gestures have no
/// bits in the key word, so what is left is the keyboard and the two mouse
/// axes. The pause bit is the engine's `sendpause`: a session sets it for
/// the one tic the key goes down, because the engine's flag is set once per
/// press and cleared as the command is built.
fn build_ticcmd(state: &State) -> Vec<(String, String)> {
    let turnheld = state.get("turnheld");
    let down = |bit: u32| format!("bitAnd(keys, {bit}) != 0");
    let table = |values: &[i32], at: &str| {
        format!(
            "[{}][1 + {at}]",
            values
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let speed_move = |values: &[i32]| table(values, "key_speed");
    let clamp = |value: &str| format!("least(greatest({value}, {}), {MAXPLMOVE})", -MAXPLMOVE);
    // The first weapon key that is down wins, as the C's loop breaks on it.
    let weapon = format!(
        "toInt32(indexOf(arrayMap(w -> bitAnd(bitShiftRight(keys, {} + w), 1), \
         range({WEAPON_KEYS})), 1) - 1)",
        key::WEAPON_SHIFT
    );
    vec![
        ("key_strafe".to_owned(), down(key::STRAFE)),
        (
            "key_speed".to_owned(),
            format!("toUInt8({})", down(key::SPEED)),
        ),
        (
            "now_turnheld".to_owned(),
            format!(
                "toInt32(if({} OR {}, {turnheld} + {TICDUP}, 0))",
                down(key::RIGHT),
                down(key::LEFT)
            ),
        ),
        (
            "key_tspeed".to_owned(),
            format!("toUInt8(if(now_turnheld < {SLOWTURNTICS}, 2, key_speed))"),
        ),
        (
            "cmd_side".to_owned(),
            format!(
                "toInt32({} + {} + {} + {} + if(key_strafe, mouse_dx * 2, 0))",
                stepped(&down(key::RIGHT), "key_strafe", &speed_move(&SIDEMOVE), 1),
                stepped(&down(key::LEFT), "key_strafe", &speed_move(&SIDEMOVE), -1),
                stepped(&down(key::STRAFE_LEFT), "1", &speed_move(&SIDEMOVE), -1),
                stepped(&down(key::STRAFE_RIGHT), "1", &speed_move(&SIDEMOVE), 1),
            ),
        ),
        (
            "cmd_angleturn".to_owned(),
            format!(
                "toInt16({} + {} + if(key_strafe, 0, -mouse_dx * 8))",
                stepped(
                    &down(key::RIGHT),
                    "NOT key_strafe",
                    &table(&ANGLETURN, "key_tspeed"),
                    -1
                ),
                stepped(
                    &down(key::LEFT),
                    "NOT key_strafe",
                    &table(&ANGLETURN, "key_tspeed"),
                    1
                ),
            ),
        ),
        (
            "cmd_forward".to_owned(),
            format!(
                "toInt32({} + {} + mouse_dy)",
                stepped(&down(key::UP), "1", &speed_move(&FORWARDMOVE), 1),
                stepped(&down(key::DOWN), "1", &speed_move(&FORWARDMOVE), -1),
            ),
        ),
        (
            "cmd_forwardmove".to_owned(),
            format!("toInt8({})", clamp("cmd_forward")),
        ),
        (
            "cmd_sidemove".to_owned(),
            format!("toInt8({})", clamp("cmd_side")),
        ),
        ("cmd_weapon".to_owned(), weapon),
        (
            "cmd_buttons".to_owned(),
            format!(
                "toUInt8(if({}, {}, \
                 if({}, {BT_ATTACK}, 0) + if({}, {BT_USE}, 0) + \
                 if(cmd_weapon >= 0, {BT_CHANGE} + bitShiftLeft(cmd_weapon, {BT_WEAPONSHIFT}), 0)))",
                down(key::PAUSE),
                BT_SPECIAL + BTS_PAUSE,
                down(key::FIRE),
                down(key::USE),
            ),
        ),
    ]
}

/// One key's contribution to a movement total: `when` gates it the way the
/// C's branch does, and `sign` is which way it carries.
fn stepped(down: &str, when: &str, step: &str, sign: i32) -> String {
    let step = if sign < 0 {
        format!("-({step})")
    } else {
        step.to_owned()
    };
    format!("if({down} AND {when}, {step}, 0)")
}

/// `G_Ticker`'s special buttons. The only one a key can send is the pause,
/// and `P_Ticker` returns before it runs anything while it is on.
pub fn special_buttons(state: &State) -> Vec<(String, String)> {
    let buttons = state.get("p_cmd_buttons");
    vec![
        (
            "game_pauses".to_owned(),
            format!(
                "bitAnd({buttons}, {BT_SPECIAL}) != 0 \
                 AND bitAnd({buttons}, {BT_SPECIALMASK}) = {BTS_PAUSE}"
            ),
        ),
        (
            "now_paused".to_owned(),
            "toUInt8(if(game_pauses, bitXor(prev_paused, 1), prev_paused))".to_owned(),
        ),
    ]
}

/// Whether `P_Ticker` runs this tic.
pub fn running(state: &State) -> String {
    format!("{} = 0", state.get("paused"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(bindings: &[(String, String)], name: &str) -> String {
        bindings
            .iter()
            .find(|(binding, _)| binding == name)
            .map(|(_, expr)| expr.clone())
            .unwrap_or_else(|| panic!("{name}"))
    }

    #[test]
    fn a_held_turn_key_turns_slowly_before_it_turns_fast() {
        let bindings = build_ticcmd(&State::default());
        assert_eq!(
            binding(&bindings, "now_turnheld"),
            format!(
                "toInt32(if(bitAnd(keys, {}) != 0 OR bitAnd(keys, {}) != 0, prev_turnheld + 1, 0))",
                key::RIGHT,
                key::LEFT
            )
        );
        assert!(binding(&bindings, "key_tspeed").contains("now_turnheld < 6"));
        assert!(binding(&bindings, "cmd_angleturn").contains("[640, 1280, 320][1 + key_tspeed]"));
    }

    #[test]
    fn the_movement_totals_are_clamped_to_one_running_step() {
        let bindings = build_ticcmd(&State::default());
        for name in ["cmd_forwardmove", "cmd_sidemove"] {
            assert!(
                binding(&bindings, name).contains("least(greatest("),
                "{name}"
            );
            assert!(binding(&bindings, name).contains("-50"), "{name}");
            assert!(binding(&bindings, name).contains(", 50)"), "{name}");
        }
    }

    #[test]
    fn strafing_moves_sideways_instead_of_turning() {
        let bindings = build_ticcmd(&State::default());
        let side = binding(&bindings, "cmd_side");
        let turn = binding(&bindings, "cmd_angleturn");
        assert!(side.contains("AND key_strafe"));
        assert!(side.contains("if(key_strafe, mouse_dx * 2, 0)"));
        assert!(turn.contains("AND NOT key_strafe"));
        assert!(turn.contains("if(key_strafe, 0, -mouse_dx * 8)"));
    }

    #[test]
    fn the_demo_overwrites_what_the_keys_built() {
        let bindings = command(&State::default(), "nat");
        let forward = binding(&bindings, "now_p_cmd_forwardmove");
        assert!(forward.contains("demo_forwardmove[tic]"));
        assert!(forward.contains("cmd_forwardmove"));
        assert!(
            !binding(&bindings, "now_turnheld").contains("demo"),
            "the demo does not carry a turn key"
        );
    }

    #[test]
    fn every_binding_balances_its_parentheses() {
        let mut bindings = command(&State::default(), "nat");
        bindings.extend(special_buttons(&State::default()));
        for (name, expr) in bindings {
            let depth = expr.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{name}");
        }
    }
}
