//! What a monster does before it has a target, from `p_enemy.c`.

use crate::sql::{Statement, fixed};

/// `p_local.h`: how close is close enough to react to something behind.
const MELEERANGE: i64 = 64 << 16;
/// `tables.h`
const ANG90: i64 = 0x4000_0000;
const ANG270: i64 = 0xC000_0000;

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

/// What stops the load: a sound `A_Look` switches on that `sfxenum` does
/// not carry. The draw for it would go missing and every random number
/// after it would be the wrong one.
pub fn guards(db: &str) -> Vec<Statement> {
    let names: Vec<String> = SEE_SOUNDS.iter().map(|name| format!("'{name}'")).collect();
    vec![Statement::sql(format!(
        "SELECT throwIf(count() != {}, 'A_Look: a sound it switches on is missing')\n\
         FROM {db}.sfxenum\n\
         WHERE name IN ({})",
        SEE_SOUNDS.len(),
        names.join(", ")
    ))]
}

/// The constants `A_Look`'s sound switch reads.
pub fn constants(db: &str) -> Vec<(String, String)> {
    let names: Vec<String> = SEE_SOUNDS.iter().map(|name| format!("'{name}'")).collect();
    vec![(
        "a_look_sounds".to_owned(),
        format!(
            "(SELECT groupArray(toInt32(id)) FROM {db}.sfxenum WHERE name IN ({}))",
            names.join(", ")
        ),
    )]
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
}
