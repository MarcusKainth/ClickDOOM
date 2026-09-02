//! The load plan, compared against a committed listing.
//!
//! What is goldened is the plan, not the schema: `native/schema.sql` is the
//! schema, and a second copy of it here would be a copy that drifts. The
//! listing carries each statement's first line and the size of the body it
//! sends, which is what a change to the statement list, its order, or the
//! bytes it streams moves.

use clickdoom_native::{load, wad::Wad};

mod support;

const GOLDEN: &str = include_str!("golden/plan.txt");

#[test]
fn the_load_plan_matches_the_committed_listing() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let actual: Vec<String> = load::plan("clickdoom_native", &wad)
        .iter()
        .map(|s| s.summary())
        .collect();
    let expected: Vec<&str> = GOLDEN.lines().collect();
    for (at, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual, expected, "statement {at}");
    }
    assert_eq!(
        actual.len(),
        expected.len(),
        "the plan has {} statements, the listing {}",
        actual.len(),
        expected.len()
    );
}

/// The WAD body is RowBinary with no framing of its own, so its size is
/// the only thing that says every lump reached it: four bytes of index and
/// a varint length per row, plus the lump bytes and the two names.
#[test]
fn the_wad_body_carries_every_lump() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let insert = clickdoom_native::sql::wad_insert("clickdoom_native", &wad);
    let payload: usize = wad
        .lumps()
        .iter()
        .map(|l| l.bytes.len() + l.name.len() + l.map_marker.len())
        .sum();
    let framing: usize = wad
        .lumps()
        .iter()
        .map(|l| {
            4 + varint_len(l.bytes.len())
                + varint_len(l.name.len())
                + varint_len(l.map_marker.len())
        })
        .sum();
    assert_eq!(insert.body.len(), payload + framing);
}

fn varint_len(value: usize) -> usize {
    let mut len = 1;
    let mut value = value >> 7;
    while value > 0 {
        len += 1;
        value >>= 7;
    }
    len
}
