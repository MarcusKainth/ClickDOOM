//! The light thinkers, from `p_lights.c`.
//!
//! `P_RunThinkers` walks the list in creation order, so a tic runs them
//! one after another and each reads the light level the one before it
//! left. That is a fold over the thinkers, and the four kinds are one
//! `multiIf` inside it.

use crate::sql::bind;
use clickdoom_spec::native_state::sector_thinker_kind as kind;

use super::State;

/// `p_spec.h`: how far a glow moves a sector's light in one tic.
const GLOWSPEED: i64 = 8;
/// `p_lights.c`: how long a fire flicker holds each level.
const FLICKER_COUNT: i64 = 4;

/// Whether the thinker of kind `kind` draws this tic. `T_LightFlash` and
/// `T_FireFlicker` each take one number when their count reaches zero;
/// `T_StrobeFlash` and `T_Glow` take none.
fn drew(kind: &str, count: &str) -> String {
    format!(
        "toUInt8({kind} IN ({}, {}) AND {count} - 1 = 0)",
        kind::LIGHT_FLASH,
        kind::FIRE_FLICKER
    )
}

/// How many numbers the sector thinkers draw this tic, as one count the
/// stages ahead of them read.
///
/// Every thinker reads its own count as the tic before left it, so this
/// is decided by the state row alone. The mobj stage runs first and needs
/// it, because a slot the sector thinkers run before counts these draws
/// in its own base.
pub fn draws(state: &State) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    vec![(
        "lt_draws".to_owned(),
        format!(
            "toUInt32(arraySum(arrayMap((k, c) -> {}, {}, {})))",
            drew("k", "c"),
            s("s_kind"),
            s("s_count"),
        ),
    )]
}

/// Where each part of the fold's accumulator sits.
mod held {
    pub const LIGHTLEVEL: usize = 1;
    pub const COUNT: usize = 2;
    pub const DIRECTION: usize = 3;
    pub const DRAWS: usize = 4;
}

/// `T_LightFlash`, `T_StrobeFlash`, `T_Glow` and `T_FireFlicker`, over
/// every sector thinker in list order.
///
/// Two of them draw from `P_Random` when their count runs out, so the
/// accumulator carries how many draws the tic has made and each thinker
/// reads the table at the index its own draw lands on. `index` is where
/// the first of them starts, which is not where the mobj stage left the
/// tic's own: the sector thinkers run partway up the list.
pub fn thinkers(state: &State, index: &str) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let at = |name: &str| format!("{}[j]", s(name));
    let sector = "1 + light_sector";
    let held = |field: usize| format!("light_at.{field}");
    let level = format!("{}[{sector}]", held(held::LIGHTLEVEL));
    let count = format!("{}[j]", held(held::COUNT));
    let direction = format!("{}[j]", held(held::DIRECTION));
    let draw = format!(
        "toInt64(rnd[1 + bitAnd(toUInt32({index}) + {} + 1, 255)])",
        held(held::DRAWS)
    );

    // Each kind's answer: the light level it leaves, its count, its
    // direction, and whether it drew.
    let values = vec![
        (
            "light_sector".to_owned(),
            format!("toInt32({})", at("s_sector")),
        ),
        ("light_kind".to_owned(), at("s_kind")),
        // The list carries the plane thinkers too, and none of what
        // follows is theirs.
        (
            "light_runs".to_owned(),
            format!(
                "toUInt8(light_kind IN ({}, {}, {}, {}))",
                kind::LIGHT_FLASH,
                kind::STROBE,
                kind::GLOW,
                kind::FIRE_FLICKER
            ),
        ),
        (
            "light_fires".to_owned(),
            format!("toUInt8({count} - 1 = 0)"),
        ),
        (
            "light_flicker".to_owned(),
            format!("toInt64(bitAnd({draw}, 3) * 16)"),
        ),
        (
            "light_level".to_owned(),
            format!(
                "toInt16(multiIf(\
                 light_kind = {glow} AND {direction} = -1 AND toInt64({level}) - {GLOWSPEED} \
                 <= toInt64({minlight}), toInt64({level}), \
                 light_kind = {glow} AND {direction} = -1, toInt64({level}) - {GLOWSPEED}, \
                 light_kind = {glow} AND {direction} != 1, toInt64({level}), \
                 light_kind = {glow} AND toInt64({level}) + {GLOWSPEED} >= toInt64({maxlight}), \
                 toInt64({level}), \
                 light_kind = {glow}, toInt64({level}) + {GLOWSPEED}, \
                 light_fires = 0, toInt64({level}), \
                 light_kind = {flash}, if({level} = {maxlight}, {minlight}, {maxlight}), \
                 light_kind = {strobe}, if({level} = {minlight}, {maxlight}, {minlight}), \
                 light_kind = {flicker} AND toInt64({level}) - light_flicker \
                 < toInt64({minlight}), toInt64({minlight}), \
                 light_kind = {flicker}, toInt64({maxlight}) - light_flicker, \
                 toInt64({level})))",
                glow = kind::GLOW,
                flash = kind::LIGHT_FLASH,
                strobe = kind::STROBE,
                flicker = kind::FIRE_FLICKER,
                minlight = at("s_minlight"),
                maxlight = at("s_maxlight"),
            ),
        ),
        (
            "light_count".to_owned(),
            format!(
                "toInt32(multiIf(\
                 light_kind = {glow}, {count}, \
                 light_fires = 0, {count} - 1, \
                 light_kind = {flash} AND {level} = {maxlight}, bitAnd({draw}, {mintime}) + 1, \
                 light_kind = {flash}, bitAnd({draw}, {maxtime}) + 1, \
                 light_kind = {strobe} AND {level} = {minlight}, {maxtime}, \
                 light_kind = {strobe}, {mintime}, \
                 light_kind = {flicker}, {FLICKER_COUNT}, \
                 {count}))",
                glow = kind::GLOW,
                flash = kind::LIGHT_FLASH,
                strobe = kind::STROBE,
                flicker = kind::FIRE_FLICKER,
                minlight = at("s_minlight"),
                maxlight = at("s_maxlight"),
                mintime = at("s_mintime"),
                maxtime = at("s_maxtime"),
            ),
        ),
        (
            "light_direction".to_owned(),
            format!(
                "toInt32(multiIf(\
                 light_kind != {glow}, {direction}, \
                 {direction} = -1 AND toInt64({level}) - {GLOWSPEED} <= toInt64({minlight}), 1, \
                 {direction} = 1 AND toInt64({level}) + {GLOWSPEED} >= toInt64({maxlight}), -1, \
                 {direction}))",
                glow = kind::GLOW,
                minlight = at("s_minlight"),
                maxlight = at("s_maxlight"),
            ),
        ),
        ("light_drew".to_owned(), drew("light_kind", &count)),
    ];
    let put = |array: String, index: &str, value: &str| {
        format!("arrayMap((v, i) -> if(i = {index}, {value}, v), {array}, arrayEnumerate({array}))")
    };
    let body = format!(
        "if(light_runs = 0, light_at, ({}, {}, {}, toUInt32({} + light_drew)))",
        put(held(held::LIGHTLEVEL), sector, "light_level"),
        put(held(held::COUNT), "j", "light_count"),
        put(held(held::DIRECTION), "j", "light_direction"),
        held(held::DRAWS)
    );
    let start = format!(
        "({}, {}, {}, toUInt32(0))",
        s("sec_lightlevel"),
        s("s_count"),
        s("s_direction")
    );
    let ran = format!(
        "arrayFold((light_at, j) -> {}, arrayEnumerate({}), {start})",
        bind::chain(&values, &body),
        s("s_kind")
    );
    vec![
        ("lights".to_owned(), ran),
        (
            "now_sec_lightlevel".to_owned(),
            format!("lights.{}", held::LIGHTLEVEL),
        ),
        ("now_s_count".to_owned(), format!("lights.{}", held::COUNT)),
        (
            "now_s_direction".to_owned(),
            format!("lights.{}", held::DIRECTION),
        ),
        (
            "now_prndindex".to_owned(),
            format!(
                "toUInt8(bitAnd(toUInt32({}) + lights.{}, 255))",
                s("prndindex"),
                held::DRAWS
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thinkers_are_one_fold_over_the_list() {
        let bindings = thinkers(&State::default(), "mt_light_index");
        let (_, ran) = bindings.iter().find(|(name, _)| name == "lights").unwrap();
        assert_eq!(ran.matches("arrayFold((light_at, j)").count(), 1);
        assert!(ran.contains("arrayEnumerate(prev_s_kind)"));
    }

    #[test]
    fn only_the_two_that_draw_move_the_random_index() {
        let bindings = thinkers(&State::default(), "mt_light_index");
        let (_, ran) = bindings.iter().find(|(name, _)| name == "lights").unwrap();
        assert!(ran.contains(&format!(
            "IN ({}, {})",
            kind::LIGHT_FLASH,
            kind::FIRE_FLICKER
        )));
    }

    /// The sector thinkers run partway up the thinker list, so the table
    /// index each one reads comes off the base it is given rather than
    /// off the index the mobj stage left.
    #[test]
    fn a_draw_reads_the_table_at_the_base_it_is_given() {
        let bindings = thinkers(&State::default(), "mt_light_index");
        let (_, ran) = bindings.iter().find(|(name, _)| name == "lights").unwrap();
        assert!(
            ran.contains("rnd[1 + bitAnd(toUInt32(mt_light_index)"),
            "{ran}"
        );
        assert!(
            !ran.contains("rnd[1 + bitAnd(toUInt32(prev_prndindex)"),
            "{ran}"
        );
        // The tic's own total still moves by what the fold drew.
        let (_, index) = bindings
            .iter()
            .find(|(name, _)| name == "now_prndindex")
            .unwrap();
        assert!(index.contains("prev_prndindex"), "{index}");
    }

    /// The count the stages ahead of the list read is the same rule the
    /// fold applies, over the counts the tic starts with.
    #[test]
    fn the_count_reads_the_state_row_alone() {
        let (name, count) = draws(&State::default()).into_iter().next().unwrap();
        assert_eq!(name, "lt_draws");
        assert_eq!(
            count,
            format!(
                "toUInt32(arraySum(arrayMap((k, c) -> {}, prev_s_kind, prev_s_count)))",
                drew("k", "c")
            )
        );
    }

    #[test]
    fn every_binding_balances_its_parentheses() {
        for (name, expr) in thinkers(&State::default(), "mt_light_index") {
            let depth = expr.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{name}");
        }
    }
}
