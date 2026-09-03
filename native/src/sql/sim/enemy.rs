//! What a monster does before it has a target, from `p_enemy.c`.

use crate::sql::fixed;

/// `p_local.h`: how close is close enough to react to something behind.
const MELEERANGE: i64 = 64 << 16;
/// `tables.h`
const ANG90: i64 = 0x4000_0000;
const ANG270: i64 = 0xC000_0000;

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
