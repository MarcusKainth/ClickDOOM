//! The status bar, the heads-up display and the menu, from `st_stuff.c`,
//! `hu_stuff.c` and `m_menu.c`.
//!
//! `G_Ticker` runs these after `P_Ticker`, so each reads the state the tic
//! has already left behind.

use crate::sql::fixed;

use super::State;

/// `st_stuff.c`: the face pictures, and where each group starts.
const NUMPAINFACES: i32 = 5;
const NUMSTRAIGHTFACES: i32 = 3;
const NUMTURNFACES: i32 = 2;
const NUMSPECIALFACES: i32 = 3;
const FACESTRIDE: i32 = NUMSTRAIGHTFACES + NUMTURNFACES + NUMSPECIALFACES;
const TURNOFFSET: i32 = NUMSTRAIGHTFACES;
const OUCHOFFSET: i32 = TURNOFFSET + NUMTURNFACES;
const EVILGRINOFFSET: i32 = OUCHOFFSET + 1;
const RAMPAGEOFFSET: i32 = EVILGRINOFFSET + 1;
const GODFACE: i32 = NUMPAINFACES * FACESTRIDE;
const DEADFACE: i32 = GODFACE + 1;

/// `st_stuff.c`: how long each expression holds, in tics.
const TICRATE: i32 = 35;
const EVILGRINCOUNT: i32 = 2 * TICRATE;
const STRAIGHTFACECOUNT: i32 = TICRATE / 2;
const TURNCOUNT: i32 = TICRATE;
const RAMPAGEDELAY: i32 = 2 * TICRATE;
const MUCHPAIN: i32 = 20;

/// `st_stuff.c`: the palettes damage and pickups tint the screen with.
const STARTREDPALS: i32 = 1;
const NUMREDPALS: i32 = 8;
const STARTBONUSPALS: i32 = 9;
const NUMBONUSPALS: i32 = 4;
const RADIATIONPAL: i32 = 13;

/// `hu_stuff.c`: how long a message stays up.
const MSGTIMEOUT: i32 = 4 * TICRATE;

/// `m_menu.c`: how long each skull frame holds.
const SKULLANIMCOUNT: i32 = 8;

/// `d_player.h`: the powers the face and the palette read, one-based for
/// the array they sit in.
const PW_INVULNERABILITY: usize = 2;
const PW_STRENGTH: usize = 3;
const PW_IRONFEET: usize = 5;

/// `d_player.h`
const CF_GODMODE: i32 = 2;

/// `tables.h`
const ANG45: i64 = 0x2000_0000;
const ANG180: i64 = 0x8000_0000;

/// `ST_Ticker`, `HU_Ticker` and `M_Ticker`, in the order `G_Ticker` runs
/// them.
pub fn tickers(state: &State) -> Vec<(String, String)> {
    let mut bindings = status_bar(state);
    bindings.extend(heads_up(state));
    bindings.extend(menu());
    bindings
}

/// `ST_Ticker`: the clock, the number the face is chosen with, the face
/// itself, and the health the next tic compares against.
///
/// `ST_doPaletteStuff` belongs to `ST_Drawer` rather than to the ticker,
/// but it is a function of the state the tic left and nothing between the
/// two moves that state, so the palette a frame picks is the one this row
/// carries.
fn status_bar(state: &State) -> Vec<(String, String)> {
    let health = state.get("p_health");
    let mut bindings = vec![
        (
            "now_st_clock".to_owned(),
            "toInt32(prev_st_clock + 1)".to_owned(),
        ),
        (
            "now_st_randomnumber".to_owned(),
            "toInt32(rnd[1 + bitAnd(toUInt32(prev_rndindex) + 1, 255)])".to_owned(),
        ),
        // `ST_calcPainOffset` recomputes only when the health has moved,
        // and returns what it last worked out when it has not.
        ("face_health".to_owned(), format!("least({health}, 100)")),
        (
            "face_offset".to_owned(),
            format!(
                "toInt32(if(face_health != prev_st_calc_oldhealth, \
                 {FACESTRIDE} * intDiv((100 - face_health) * {NUMPAINFACES}, 101), \
                 prev_st_lastcalc))"
            ),
        ),
    ];
    bindings.extend(face_widget(state));
    bindings.extend([
        ("now_st_oldhealth".to_owned(), format!("toInt32({health})")),
        ("now_st_palette".to_owned(), palette(state)),
    ]);
    bindings
}

/// `ST_updateFaceWidget`: a ladder of expressions, the highest priority
/// first, ending in a look left or right once the last one has run out.
///
/// Each rung leaves the triple the C leaves, and a rung that does not fire
/// passes the rung above it through. `ST_calcPainOffset` is called from
/// five of the rungs and nowhere else, so the offset it caches only moves
/// on a tic where one of them fires.
fn face_widget(state: &State) -> Vec<(String, String)> {
    let health = state.get("p_health");
    let attacker = state.get("p_attacker");
    let mo = state.get("p_mo");
    let (mx, my, angle) = (state.get("m_x"), state.get("m_y"), state.get("m_angle"));
    let badguy = fixed::point_to_angle(
        &format!("toInt32({mx}[{attacker}] - {mx}[{mo}])"),
        &format!("toInt32({my}[{attacker}] - {my}[{mo}])"),
        "tantoangle",
    );
    let much_pain = format!("{health} - prev_st_oldhealth > {MUCHPAIN}");
    vec![
        (
            "face_diffang".to_owned(),
            format!(
                "toUInt32(if({badguy} > {angle}[{mo}], \
                 {badguy} - {angle}[{mo}], {angle}[{mo}] - {badguy}))"
            ),
        ),
        (
            "face_turned".to_owned(),
            format!(
                "toUInt8(if({badguy} > {angle}[{mo}], \
                 face_diffang > {ANG180}, face_diffang <= {ANG180}))"
            ),
        ),
        // Dead.
        (
            "face_dead".to_owned(),
            format!("prev_st_priority < 10 AND {health} = 0"),
        ),
        (
            "face_p1".to_owned(),
            "toInt32(if(face_dead, 9, prev_st_priority))".to_owned(),
        ),
        (
            "face_f1".to_owned(),
            format!("toInt32(if(face_dead, {DEADFACE}, prev_st_faceindex))"),
        ),
        (
            "face_c1".to_owned(),
            "toInt32(if(face_dead, 1, prev_st_facecount))".to_owned(),
        ),
        // Picking something up, and grinning if it was a weapon.
        (
            "face_bonus".to_owned(),
            format!("face_p1 < 9 AND {} != 0", state.get("p_bonuscount")),
        ),
        (
            "now_st_oldweaponsowned".to_owned(),
            format!(
                "if(face_bonus, {}, prev_st_oldweaponsowned)",
                state.get("p_weaponowned")
            ),
        ),
        (
            "face_grin".to_owned(),
            format!(
                "face_bonus AND prev_st_oldweaponsowned != {}",
                state.get("p_weaponowned")
            ),
        ),
        (
            "face_p2".to_owned(),
            "toInt32(if(face_grin, 8, face_p1))".to_owned(),
        ),
        (
            "face_f2".to_owned(),
            format!("toInt32(if(face_grin, face_offset + {EVILGRINOFFSET}, face_f1))"),
        ),
        (
            "face_c2".to_owned(),
            format!("toInt32(if(face_grin, {EVILGRINCOUNT}, face_c1))"),
        ),
        // Being attacked by something else.
        (
            "face_hit".to_owned(),
            format!(
                "face_p2 < 8 AND {} != 0 AND {attacker} != 0 AND {attacker} != {mo}",
                state.get("p_damagecount")
            ),
        ),
        (
            "face_p3".to_owned(),
            "toInt32(if(face_hit, 7, face_p2))".to_owned(),
        ),
        (
            "face_f3".to_owned(),
            format!(
                "toInt32(if(face_hit, if({much_pain}, face_offset + {OUCHOFFSET}, \
                 face_offset + multiIf(face_diffang < {ANG45}, {RAMPAGEOFFSET}, \
                 face_turned != 0, {TURNOFFSET}, {TURNOFFSET} + 1)), face_f2))"
            ),
        ),
        (
            "face_c3".to_owned(),
            format!("toInt32(if(face_hit, {TURNCOUNT}, face_c2))"),
        ),
        // Hurt by something the player did.
        (
            "face_hurt".to_owned(),
            format!("face_p3 < 7 AND {} != 0", state.get("p_damagecount")),
        ),
        (
            "face_p4".to_owned(),
            format!("toInt32(if(face_hurt, if({much_pain}, 7, 6), face_p3))"),
        ),
        (
            "face_f4".to_owned(),
            format!(
                "toInt32(if(face_hurt, face_offset + \
                 if({much_pain}, {OUCHOFFSET}, {RAMPAGEOFFSET}), face_f3))"
            ),
        ),
        (
            "face_c4".to_owned(),
            format!("toInt32(if(face_hurt, {TURNCOUNT}, face_c3))"),
        ),
        // Firing without a break for long enough to look angry.
        (
            "face_rapid".to_owned(),
            format!("face_p4 < 6 AND {} != 0", state.get("p_attackdown")),
        ),
        (
            "face_rampage".to_owned(),
            "face_rapid AND prev_st_lastattackdown = 1".to_owned(),
        ),
        (
            "now_st_lastattackdown".to_owned(),
            format!(
                "toInt32(multiIf(\
                 face_p4 >= 6, prev_st_lastattackdown, \
                 {} = 0, -1, \
                 face_rampage, 1, \
                 prev_st_lastattackdown = -1, {RAMPAGEDELAY}, \
                 prev_st_lastattackdown - 1))",
                state.get("p_attackdown")
            ),
        ),
        (
            "face_p5".to_owned(),
            "toInt32(if(face_rampage, 5, face_p4))".to_owned(),
        ),
        (
            "face_f5".to_owned(),
            format!("toInt32(if(face_rampage, face_offset + {RAMPAGEOFFSET}, face_f4))"),
        ),
        (
            "face_c5".to_owned(),
            "toInt32(if(face_rampage, 1, face_c4))".to_owned(),
        ),
        // Untouchable.
        (
            "face_god".to_owned(),
            format!(
                "face_p5 < 5 AND (bitAnd({}, {CF_GODMODE}) != 0 OR {}[{PW_INVULNERABILITY}] != 0)",
                state.get("p_cheats"),
                state.get("p_powers")
            ),
        ),
        (
            "face_p6".to_owned(),
            "toInt32(if(face_god, 4, face_p5))".to_owned(),
        ),
        (
            "face_f6".to_owned(),
            format!("toInt32(if(face_god, {GODFACE}, face_f5))"),
        ),
        (
            "face_c6".to_owned(),
            "toInt32(if(face_god, 1, face_c5))".to_owned(),
        ),
        // Nothing to say: look left, right or ahead.
        ("face_straight".to_owned(), "face_c6 = 0".to_owned()),
        (
            "now_st_priority".to_owned(),
            "toInt32(if(face_straight, 0, face_p6))".to_owned(),
        ),
        (
            "now_st_faceindex".to_owned(),
            "toInt32(if(face_straight, face_offset + now_st_randomnumber % 3, face_f6))".to_owned(),
        ),
        (
            "now_st_facecount".to_owned(),
            format!("toInt32(if(face_straight, {STRAIGHTFACECOUNT}, face_c6) - 1)"),
        ),
        (
            "face_calls_offset".to_owned(),
            "face_grin OR face_hit OR face_hurt OR face_rampage OR face_straight".to_owned(),
        ),
        (
            "now_st_lastcalc".to_owned(),
            "toInt32(if(face_calls_offset, face_offset, prev_st_lastcalc))".to_owned(),
        ),
        (
            "now_st_calc_oldhealth".to_owned(),
            "toInt32(if(face_calls_offset, face_health, prev_st_calc_oldhealth))".to_owned(),
        ),
    ]
}

/// `ST_doPaletteStuff`: damage reds, then pickup golds, then the radiation
/// suit's green.
fn palette(state: &State) -> String {
    let powers = state.get("p_powers");
    let strength = format!("{powers}[{PW_STRENGTH}]");
    let ironfeet = format!("{powers}[{PW_IRONFEET}]");
    let count = format!(
        "greatest({}, if({strength} != 0, 12 - bitShiftRight({strength}, 6), 0))",
        state.get("p_damagecount")
    );
    let bonus = state.get("p_bonuscount");
    format!(
        "toInt32(multiIf(\
         {count} != 0, least(bitShiftRight({count} + 7, 3), {}) + {STARTREDPALS}, \
         {bonus} != 0, least(bitShiftRight({bonus} + 7, 3), {}) + {STARTBONUSPALS}, \
         {ironfeet} > 128 OR bitAnd({ironfeet}, 8) != 0, {RADIATIONPAL}, \
         0))",
        NUMREDPALS - 1,
        NUMBONUSPALS - 1
    )
}

/// `HU_Ticker`: the message counter runs down, and then the widget takes
/// whatever message the player is holding.
fn heads_up(state: &State) -> Vec<(String, String)> {
    let message = state.get("p_message");
    vec![
        (
            "hu_ran_out".to_owned(),
            "prev_hu_message_counter != 0 AND prev_hu_message_counter = 1".to_owned(),
        ),
        (
            "hu_takes".to_owned(),
            format!("{message} != 0 AND prev_hu_nottobefuckedwith = 0"),
        ),
        (
            "now_hu_message_counter".to_owned(),
            format!(
                "toInt32(multiIf(hu_takes, {MSGTIMEOUT}, \
                 prev_hu_message_counter != 0, prev_hu_message_counter - 1, \
                 prev_hu_message_counter))"
            ),
        ),
        (
            "now_hu_message_on".to_owned(),
            "toUInt8(multiIf(hu_takes, 1, hu_ran_out, 0, prev_hu_message_on))".to_owned(),
        ),
        (
            "now_hu_nottobefuckedwith".to_owned(),
            "toUInt8(multiIf(hu_takes, 0, hu_ran_out, 0, prev_hu_nottobefuckedwith))".to_owned(),
        ),
        (
            "now_hu_message".to_owned(),
            format!("toUInt64(if(hu_takes, {message}, prev_hu_message))"),
        ),
        (
            "now_p_message".to_owned(),
            format!("toUInt64(if(hu_takes, 0, {message}))"),
        ),
    ]
}

/// `M_Ticker`: the skull beside the menu's selection blinks whether the
/// menu is up or not.
fn menu() -> Vec<(String, String)> {
    let ran_out = "prev_menu_skullanim - 1 <= 0";
    vec![
        (
            "now_menu_skullanim".to_owned(),
            format!("toInt32(if({ran_out}, {SKULLANIMCOUNT}, prev_menu_skullanim - 1))"),
        ),
        (
            "now_menu_whichskull".to_owned(),
            format!(
                "toInt32(if({ran_out}, bitXor(prev_menu_whichskull, 1), prev_menu_whichskull))"
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_face_ladder_reads_the_rung_above_it() {
        let bindings = face_widget(&State::default());
        let at = |name: &str| {
            bindings
                .iter()
                .position(|(binding, _)| binding == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        assert!(at("face_p1") < at("face_p2"));
        assert!(at("face_p5") < at("face_p6"));
        assert!(at("face_p6") < at("now_st_priority"));
        assert!(at("now_st_priority") < at("face_calls_offset"));
    }

    #[test]
    fn the_offset_moves_only_when_a_rung_asked_for_it() {
        let bindings = face_widget(&State::default());
        let (_, expr) = bindings
            .iter()
            .find(|(name, _)| name == "now_st_lastcalc")
            .unwrap();
        assert_eq!(
            expr,
            "toInt32(if(face_calls_offset, face_offset, prev_st_lastcalc))"
        );
    }

    #[test]
    fn a_stage_that_has_not_run_reads_the_tic_before_it() {
        let bindings = tickers(&State::default());
        let (_, health) = bindings
            .iter()
            .find(|(name, _)| name == "now_st_oldhealth")
            .unwrap();
        assert_eq!(health, "toInt32(prev_p_health)");
    }

    #[test]
    fn every_binding_balances_its_parentheses() {
        for (name, expr) in tickers(&State::default()) {
            let depth = expr.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{name}");
        }
    }
}
