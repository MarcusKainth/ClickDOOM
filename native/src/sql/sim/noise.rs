//! The alert a shot sends through the sectors, from `p_enemy.c`.
//!
//! `P_NoiseAlert` floods out from the sector the noise was made in and
//! leaves every sector it reaches holding the thing that made it.
//! `P_RecursiveSound` counts the sound-blocking lines it has crossed and
//! will not cross a second, so a sector the noise reaches has one or none
//! behind it and `soundtraversed` holds that count plus one.
//!
//! The walk is written as two floods rather than as a recursion. Its
//! "already flooded" test lets a sector be walked again whenever the count
//! it arrives with is smaller than the one it holds, so the count a sector
//! ends on is the smallest any way in offers, whatever order the walk
//! takes. The sectors that end on 1 are then the ones reachable across no
//! blocking line, and the rest of the reached ones are what a second flood
//! finds from the far side of one.
//!
//! It reads the line tables [`maputl::constants`] binds and needs none of
//! its own.

use crate::sql::{Statement, bind};

use super::maputl;

/// `doomdata.h`
const ML_TWOSIDED: i64 = 4;
const ML_SOUNDBLOCK: i64 = 64;

/// What `P_RecursiveSound` writes into `soundtraversed`: `soundblocks + 1`,
/// which is 1 until the walk crosses a sound-blocking line and 2 after it.
const NEAR: i64 = 1;
const FAR: i64 = 2;

/// What stops the load: a line the flood would cross with no sector on the
/// far side.
///
/// `P_RecursiveSound` reads `sides[check->sidenum[1]].sector` for every
/// line carrying `ML_TWOSIDED`, so such a line is one the engine itself
/// walks off the end of.
pub fn guards(db: &str) -> Vec<Statement> {
    vec![Statement::sql(format!(
        "SELECT throwIf(count() != 0, 'P_RecursiveSound: a two-sided line has one side')\n\
         FROM {db}.lv_lines\n\
         WHERE bitAnd(flags, {ML_TWOSIDED}) != 0 AND sector1 = -1"
    ))]
}

/// `P_NoiseAlert` from `sector`: what each sector's `soundtraversed` holds
/// after it, and 0 for a sector the noise does not reach.
///
/// A sector the flood does not reach keeps the count and the sound target
/// it already held, because the walk writes only where it goes and
/// `validcount` keeps one alert's writes out of the next one's way.
pub fn alert(sector: &str, floorheight: &str, ceilingheight: &str) -> String {
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    // Every line the flood may cross, as one entry per direction: the
    // sector it leads out of, the sector it leads into, and whether it
    // blocks sound. All three come from constants.
    value(
        "nz_ways",
        format!(
            "arrayFilter(nz_l -> bitAnd(line_flags[1 + nz_l], {ML_TWOSIDED}) != 0, \
             range(length(line_flags)))"
        ),
    );
    let ends = |side: &str, other: &str| {
        format!(
            "arrayConcat(arrayMap(nz_l -> toInt32(line_{side}[1 + nz_l]), nz_ways), \
             arrayMap(nz_l -> toInt32(line_{other}[1 + nz_l]), nz_ways))"
        )
    };
    value("nz_from", ends("front", "back"));
    value("nz_to", ends("back", "front"));
    value(
        "nz_blocks",
        format!(
            "arrayMap(nz_l -> toUInt8(bitAnd(line_flags[1 + nz_l], {ML_SOUNDBLOCK}) != 0), \
             arrayConcat(nz_ways, nz_ways))"
        ),
    );
    // `P_LineOpening` decides whether the walk crosses, and nothing inside
    // the flood moves a plane, so a line is opened once and both of its
    // directions read that answer.
    let opening = maputl::opening("nz_l", floorheight, ceilingheight);
    let open = bind::chain_in(
        "nzo",
        &[("nz_open".to_owned(), opening)],
        "toUInt8(nz_open.1 - nz_open.2 > 0)",
    );
    value(
        "nz_half_opens",
        format!("arrayMap(nz_l -> {open}, nz_ways)"),
    );
    value(
        "nz_opens",
        "arrayConcat(nz_half_opens, nz_half_opens)".to_owned(),
    );
    // One round per sector, which is past the longest path a flood can
    // take: a path reaching a sector twice reached it sooner the first
    // time. The heights are one value per sector, so their length is the
    // count.
    value("nz_rounds", format!("range(length({floorheight}))"));

    // The sector the noise was made in, then everything a walk crossing no
    // blocking line reaches from it.
    value("nz_source", format!("[toInt32({sector})]"));
    value(
        "nz_near",
        format!("{}.1", spread("nzn", "nz_source", "nz_source")),
    );
    // One blocking line out of what that flood reached, then everything a
    // walk from there reaches without crossing another.
    value(
        "nz_seeds",
        format!(
            "arrayFilter(nz_o -> NOT has(nz_near, nz_o), {})",
            crossings("1", "nz_near")
        ),
    );
    value(
        "nz_far",
        format!(
            "arraySlice({}.1, 1 + length(nz_near))",
            spread("nzf", "arrayConcat(nz_near, nz_seeds)", "nz_seeds")
        ),
    );

    bind::chain_in(
        "nz",
        &values,
        &format!(
            "arrayMap(nz_s -> toUInt8(multiIf(has(nz_near, toInt32(nz_s)), {NEAR}, \
             has(nz_far, toInt32(nz_s)), {FAR}, 0)), range(length({floorheight})))"
        ),
    )
}

/// The sectors one crossing out of `sectors`, over the open lines whose
/// `ML_SOUNDBLOCK` flag is `blocks`.
fn crossings(blocks: &str, sectors: &str) -> String {
    format!(
        "arrayDistinct(arrayFilter((nz_t, nz_f, nz_b, nz_o) -> \
         nz_o = 1 AND nz_b = {blocks} AND has({sectors}, nz_f), \
         nz_to, nz_from, nz_blocks, nz_opens))"
    )
}

/// Every sector `frontier` reaches across open lines that do not block
/// sound, appended to `reached` in the order the flood finds them.
///
/// Each round steps once out of what the round before it added and drops
/// what the flood already holds, so a round past the end of the flood adds
/// nothing.
fn spread(prefix: &str, reached: &str, frontier: &str) -> String {
    let at = format!("{prefix}_at");
    let step = format!(
        "arrayFilter(nz_o -> NOT has({at}.1, nz_o), {})",
        crossings("0", &format!("{at}.2"))
    );
    let next = format!("{prefix}_next");
    let body = bind::chain_in(
        prefix,
        &[(next.clone(), step)],
        &format!("(arrayConcat({at}.1, {next}), {next})"),
    );
    format!("arrayFold(({at}, {prefix}_round) -> {body}, nz_rounds, ({reached}, {frontier}))")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flood() -> String {
        alert("nz_sector", "sec_floorheight", "sec_ceilingheight")
    }

    /// Both floods run one round per sector, which is past the longest
    /// path either can take. A shorter run would leave a sector the noise
    /// reaches the long way round unmarked.
    #[test]
    fn each_flood_runs_one_round_per_sector() {
        let sql = flood();
        assert_eq!(sql.matches("arrayFold(").count(), 2, "{sql}");
        assert_eq!(sql.matches("range(length(sec_floorheight))").count(), 2);
    }

    /// Every round reads the fold's own parameter. A body that read
    /// neither would be evaluated outside the fold whatever the fold did.
    #[test]
    fn a_round_reads_the_flood_it_is_given() {
        for prefix in ["nzn", "nzf"] {
            let sql = spread(prefix, "start", "start");
            assert!(sql.contains(&format!("has({prefix}_at.2, nz_f)")), "{sql}");
            assert!(sql.contains(&format!("has({prefix}_at.1, nz_o)")), "{sql}");
        }
    }

    /// The crossings are opened once for the whole flood, and the two
    /// directions of a line read that one answer. `P_LineOpening` names
    /// the ceiling of each side, so one opening is two mentions of it.
    #[test]
    fn a_line_is_opened_once_for_both_directions() {
        let sql = flood();
        assert_eq!(sql.matches("sec_ceilingheight").count(), 2, "{sql}");
    }

    #[test]
    fn the_guard_names_the_line_it_stops_for() {
        let sql = &guards("nat")[0].sql;
        assert!(sql.contains("nat.lv_lines"), "{sql}");
        assert!(sql.contains("sector1 = -1"), "{sql}");
    }

    #[test]
    fn every_builder_balances_its_parentheses() {
        for text in [flood(), guards("nat")[0].sql.clone()] {
            let depth = text.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{text}");
        }
    }
}
