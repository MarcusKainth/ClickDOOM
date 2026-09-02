//! Malformed WADs, built here rather than committed as fixtures.
//!
//! `native/tests/wad_doom1.rs` reads the real `doom1.wad` and covers the
//! well-formed case. These cover what the real file never shows: a bad
//! magic, a truncated header, an entry pointing outside the file, a map
//! lump nothing encloses.

use super::*;

/// A WAD whose directory sits after the lump bodies, as a real one does.
/// `lumps` are `(name, body)` pairs; a name shorter than 8 bytes is NUL
/// padded.
fn build(magic: &[u8; 4], lumps: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = magic.to_vec();
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    let mut entries = Vec::new();
    for (name, body) in lumps {
        let offset = out.len() as i32;
        out.extend_from_slice(body);
        entries.push((offset, body.len() as i32, *name));
    }
    let directory = out.len() as i32;
    for (offset, size, name) in &entries {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        let mut field = [0u8; name::NAME_LEN];
        field[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&field);
    }
    out[4..8].copy_from_slice(&(entries.len() as i32).to_le_bytes());
    out[8..12].copy_from_slice(&directory.to_le_bytes());
    out
}

const MAP: [(&str, &[u8]); 3] = [("E1M7", b""), ("THINGS", b"0123456789"), ("SEGS", b"")];

#[test]
fn a_map_lump_carries_its_marker() {
    let bytes = build(b"IWAD", &MAP);
    let wad = Wad::parse(&bytes).unwrap();
    assert_eq!(wad.kind(), WadKind::Iwad);
    let names: Vec<_> = wad
        .lumps()
        .iter()
        .map(|l| (l.index, l.name, l.map_marker))
        .collect();
    assert_eq!(
        names,
        [
            (0, "E1M7", "E1M7"),
            (1, "THINGS", "E1M7"),
            (2, "SEGS", "E1M7")
        ]
    );
    assert_eq!(wad.map_lump("E1M7", "THINGS").unwrap().bytes, b"0123456789");
}

#[test]
fn a_lump_no_map_owns_clears_the_marker() {
    let bytes = build(
        b"PWAD",
        &[("E1M7", b""), ("SEGS", b""), ("PNAMES", b""), ("SEGS", b"")],
    );
    let err = Wad::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, WadError::OrphanMapLump { index: 3, .. }),
        "{err}"
    );
}

#[test]
fn markers_separate_two_maps_that_share_lump_names() {
    let bytes = build(
        b"IWAD",
        &[
            ("E1M1", b""),
            ("SEGS", b"one"),
            ("E1M7", b""),
            ("SEGS", b"seven"),
        ],
    );
    let wad = Wad::parse(&bytes).unwrap();
    assert_eq!(wad.map_lump("E1M1", "SEGS").unwrap().bytes, b"one");
    assert_eq!(wad.map_lump("E1M7", "SEGS").unwrap().bytes, b"seven");
    assert_eq!(wad.map("E1M7").count(), 2);
}

#[test]
fn a_short_buffer_is_not_a_header() {
    let err = Wad::parse(b"IWAD\0\0\0").unwrap_err();
    assert!(matches!(err, WadError::HeaderTooShort { len: 7 }), "{err}");
}

#[test]
fn a_foreign_magic_is_refused() {
    let mut bytes = build(b"IWAD", &MAP);
    bytes[..4].copy_from_slice(b"ZWAD");
    let err = Wad::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, WadError::BadMagic { magic } if &magic == b"ZWAD"),
        "{err}"
    );
}

#[test]
fn a_directory_past_the_end_is_refused() {
    let mut bytes = build(b"IWAD", &MAP);
    let len = bytes.len() as i32;
    bytes[8..12].copy_from_slice(&(len - 8).to_le_bytes());
    let err = Wad::parse(&bytes).unwrap_err();
    assert!(matches!(err, WadError::DirectoryOutOfRange { .. }), "{err}");
}

#[test]
fn a_lump_count_that_overflows_the_span_is_refused() {
    let mut bytes = build(b"IWAD", &MAP);
    bytes[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
    let err = Wad::parse(&bytes).unwrap_err();
    assert!(matches!(err, WadError::DirectoryOutOfRange { .. }), "{err}");
}

#[test]
fn a_negative_lump_count_is_refused() {
    let mut bytes = build(b"IWAD", &MAP);
    bytes[4..8].copy_from_slice(&(-1i32).to_le_bytes());
    let err = Wad::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, WadError::NegativeLumpCount { count: -1 }),
        "{err}"
    );
}

#[test]
fn a_lump_body_past_the_end_is_refused() {
    let mut bytes = build(b"IWAD", &MAP);
    let directory = bytes.len() - 3 * ENTRY_LEN;
    let past_the_end = bytes.len() as i32;
    bytes[directory + ENTRY_LEN + 4..directory + ENTRY_LEN + 8]
        .copy_from_slice(&past_the_end.to_le_bytes());
    let err = Wad::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, WadError::LumpOutOfRange { index: 1, .. }),
        "{err}"
    );
}

#[test]
fn a_negative_lump_size_is_refused() {
    let mut bytes = build(b"IWAD", &MAP);
    let directory = bytes.len() - 3 * ENTRY_LEN;
    bytes[directory + ENTRY_LEN + 4..directory + ENTRY_LEN + 8]
        .copy_from_slice(&(-1i32).to_le_bytes());
    let err = Wad::parse(&bytes).unwrap_err();
    assert!(
        matches!(err, WadError::NegativeLumpExtent { index: 1, .. }),
        "{err}"
    );
}

#[test]
fn a_verified_parse_refuses_the_wrong_bytes() {
    let bytes = build(b"IWAD", &MAP);
    let err = Wad::parse_verified(&bytes, DOOM1_SHA256SUM).unwrap_err();
    assert!(matches!(err, WadError::ChecksumMismatch { .. }), "{err}");
}
