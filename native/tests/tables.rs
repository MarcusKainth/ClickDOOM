//! The committed constant tables against the vendored source they come
//! from, and against the engine's own use of them.
//!
//! The first test is the one that matters: regenerating every table from
//! `rom/vendor/doomgeneric/` has to produce the committed bytes. A table
//! that cannot be traced back to the source does not belong in the tree,
//! and a hand-edited one would fail here.
//!
//! The rest check what the reader could get wrong without the byte
//! comparison noticing: an index that points nowhere, a table read at the
//! wrong scale, a name list out of step with the numbers that index it.

use std::path::{Path, PathBuf};

use clickdoom_native::tables::{self, generate};

fn vendor() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rom/vendor/doomgeneric/doomgeneric")
}

/// A directory of this process's own, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(case: &str) -> Scratch {
        let path =
            std::env::temp_dir().join(format!("clickdoom-tables-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Scratch(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn regenerating_from_the_vendored_source_matches_what_is_committed() {
    let scratch = Scratch::new("regen");
    let written = generate::write_all(&vendor(), &scratch.0).unwrap();
    assert_eq!(written.len(), tables::TABLES.len());
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("tables");
    for name in written {
        let fresh = std::fs::read_to_string(scratch.0.join(&name)).unwrap();
        let old = std::fs::read_to_string(committed.join(&name)).unwrap();
        assert_eq!(
            fresh.len(),
            old.len(),
            "{name} regenerates to a different length"
        );
        let at = fresh
            .bytes()
            .zip(old.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(fresh.len());
        assert!(
            at == fresh.len(),
            "{name} differs at byte {at}: {:?} against {:?}",
            &fresh[at.saturating_sub(40)..(at + 40).min(fresh.len())],
            &old[at.saturating_sub(40)..(at + 40).min(old.len())],
        );
    }
}

#[test]
fn the_embedded_text_is_the_committed_text() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("tables");
    for embedded in &tables::TABLES {
        let path = committed.join(format!("{}.tsv", embedded.name));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), embedded.tsv);
    }
}

/// `P_Random` masks its index with 0xff and returns the entry as a byte,
/// so the table is exactly as long as the mask allows.
#[test]
fn rndtable_is_the_engines_own_256_entries() {
    let rows = tables::table("rndtable").unwrap();
    let values = rows.ints("value").unwrap();
    assert_eq!(values.len(), 256);
    assert_eq!(values[..3], [0, 8, 109]);
    assert!(values.iter().all(|v| (0..256).contains(v)));
}

#[test]
fn every_state_transition_lands_on_a_state() {
    let rows = tables::table("states").unwrap();
    let count = rows.rows.len() as i64;
    assert_eq!(count, 967);
    for next in rows.ints("nextstate").unwrap() {
        assert!(
            (0..count).contains(&next),
            "nextstate {next} is not a state"
        );
    }
    let actions = tables::table("action_functions").unwrap().rows.len() as i64;
    for action in rows.ints("action").unwrap() {
        assert!((0..actions).contains(&action), "action {action} has no row");
    }
}

#[test]
fn every_sprite_number_names_a_sprite() {
    let names = tables::table("sprnames").unwrap();
    assert_eq!(names.rows.len(), 138);
    let count = names.rows.len() as i64;
    for sprite in tables::table("states").unwrap().ints("sprite").unwrap() {
        assert!((0..count).contains(&sprite), "sprite {sprite} has no name");
    }
    assert!(
        names.texts("name").unwrap().iter().all(|n| n.len() == 4
            && n.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())),
        "a sprite name is not four upper-case characters"
    );
}

/// `finesine` is a 16.16 sine over `FINEANGLES` = 8,192 steps, so a
/// quarter turn is index 2,048. `m_fixed.h` puts `FRACUNIT` at 65,536 and
/// the table peaks one below it.
#[test]
fn finesine_peaks_a_unit_below_fracunit_at_a_quarter_turn() {
    let values = tables::table("finesine").unwrap().ints("value").unwrap();
    assert_eq!(values.len(), 10_240);
    assert_eq!(values[2048], 65_535);
    assert_eq!(*values.iter().max().unwrap(), 65_535);
    assert_eq!(*values.iter().min().unwrap(), -65_535);
    // The table runs a quarter turn past a full one so `finecosine` can
    // be `finesine` offset by 2,048. That overlapping quarter carries its
    // own rounding and is not a copy of the first, so a table trimmed to
    // one period would return different values.
    let differing = values[..2048]
        .iter()
        .zip(&values[8192..])
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(differing, 12, "the overlapping quarter turn changed");
}

/// `tantoangle` maps a slope to a binary angle, and its last entry is the
/// 45 degrees a slope of one stands for.
#[test]
fn tantoangle_ends_at_a_binary_45_degrees() {
    let values = tables::table("tantoangle").unwrap().ints("value").unwrap();
    assert_eq!(values.len(), 2049);
    assert_eq!(values[0], 0);
    assert_eq!(values[2048], 0x2000_0000);
    assert!(values.windows(2).all(|w| w[0] <= w[1]), "not monotonic");
}

/// `R_DrawFuzzColumn` adds an entry of `fuzzoffset` to a framebuffer
/// pointer, so every entry is one screen row up or down.
#[test]
fn fuzzoffset_is_one_screen_row_either_way() {
    let values = tables::table("fuzzoffset").unwrap().ints("value").unwrap();
    assert_eq!(values.len(), 50);
    assert!(values.iter().all(|v| v.abs() == 320));
}

#[test]
fn gammatable_is_five_levels_of_256_bytes() {
    let rows = tables::table("gammatable").unwrap();
    assert_eq!(rows.rows.len(), 5 * 256);
    let levels = rows.ints("level").unwrap();
    assert_eq!(levels.first(), Some(&0));
    assert_eq!(levels.last(), Some(&4));
    assert!(
        rows.ints("value")
            .unwrap()
            .iter()
            .all(|v| (0..256).contains(v))
    );
}

/// `mobjinfo` holds `fixed_t` sizes, which are whole map units scaled by
/// `FRACUNIT`. The player is 16 units across and 56 tall.
#[test]
fn mobjinfo_sizes_are_whole_map_units_in_fixed_point() {
    let rows = tables::table("mobjinfo").unwrap();
    assert_eq!(rows.rows.len(), 137);
    let radius = rows.ints("radius").unwrap();
    let height = rows.ints("height").unwrap();
    assert_eq!((radius[0], height[0]), (16 * 65536, 56 * 65536));
    assert!(radius.iter().chain(&height).all(|v| v % 65536 == 0));
    // MT_PLAYER: MF_SOLID|MF_SHOOTABLE|MF_DROPOFF|MF_PICKUP|MF_NOTDMATCH.
    assert_eq!(
        rows.ints("flags").unwrap()[0],
        2 | 4 | 0x400 | 0x800 | 0x200_0000
    );
}

/// `R_CheckBBox` indexes `checkcoord` by a box position it computes, and
/// the positions it never produces are left as partial initializers C
/// zero-fills. The table carries those rows so the indexing stays right.
#[test]
fn checkcoord_carries_its_zero_filled_rows() {
    let rows = tables::table("checkcoord").unwrap();
    assert_eq!(rows.rows.len(), 12);
    for (row, cells) in rows.rows.iter().enumerate() {
        let zero = cells[1..].iter().all(|c| *c == "0");
        assert_eq!(zero, [3, 5, 7, 11].contains(&row), "row {row}");
    }
}

/// Both name lists end with a terminator the engine stops at.
#[test]
fn the_name_lists_keep_their_terminators() {
    let anims = tables::table("animdefs").unwrap();
    assert_eq!(anims.ints("istexture").unwrap().last(), Some(&-1));
    let switches = tables::table("switchlist").unwrap();
    assert_eq!(switches.texts("name1").unwrap().last(), Some(&""));
    assert_eq!(switches.ints("episode").unwrap().last(), Some(&0));
}

#[test]
fn weaponinfo_states_all_exist() {
    let states = tables::table("states").unwrap().rows.len() as i64;
    let rows = tables::table("weaponinfo").unwrap();
    assert_eq!(rows.rows.len(), 9);
    for column in [
        "upstate",
        "downstate",
        "readystate",
        "atkstate",
        "flashstate",
    ] {
        for state in rows.ints(column).unwrap() {
            assert!(
                (0..states).contains(&state),
                "{column} {state} is not a state"
            );
        }
    }
}
