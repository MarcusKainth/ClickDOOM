//! The WAD reader against the shareware `doom1.wad` the repository ships.
//!
//! The numbers here are the file's own, read back from its directory. A
//! reader that mislays a lump, misreads a size or loses a map marker fails
//! one of them.

use clickdoom_native::wad::{DOOM1_SHA256SUM, Wad, WadKind, marker::MAP_LUMPS};

fn doom1() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../rom/wad/doom1.wad");
    std::fs::read(path).expect("rom/wad/doom1.wad is committed")
}

#[test]
fn header_and_directory_match_the_shipped_file() {
    let bytes = doom1();
    assert_eq!(bytes.len(), 4_196_020);
    let wad = Wad::parse_verified(&bytes, DOOM1_SHA256SUM).unwrap();
    assert_eq!(wad.kind(), WadKind::Iwad);
    assert_eq!(wad.lumps().len(), 1264);
    let total: usize = wad.lumps().iter().map(|l| l.bytes.len()).sum();
    assert_eq!(total, 4_175_556);
}

#[test]
fn a_map_lump_name_is_unique_within_its_map() {
    let bytes = doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let mut seen = std::collections::HashSet::new();
    for lump in wad.lumps().iter().filter(|l| !l.map_marker.is_empty()) {
        assert!(
            seen.insert((lump.map_marker, lump.name)),
            "{} repeats inside {}",
            lump.name,
            lump.map_marker
        );
    }
}

/// The name a lump is looked up by is not unique in the file, so a lookup
/// has to say which one it means. The engine takes the last, and so does
/// [`Wad::find`].
#[test]
fn a_repeated_name_resolves_to_the_last_lump_carrying_it() {
    let bytes = doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let sw18_7: Vec<_> = wad
        .lumps()
        .iter()
        .filter(|l| l.name == "SW18_7")
        .map(|l| l.index)
        .collect();
    assert_eq!(sw18_7.len(), 2, "doom1.wad holds two lumps named SW18_7");
    assert_eq!(wad.find("SW18_7").unwrap().index, sw18_7[1]);
}

#[test]
fn e1m7_holds_the_eleven_map_lumps_in_wad_order() {
    let bytes = doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let names: Vec<_> = wad.map("E1M7").map(|l| l.name).collect();
    let mut expected = vec!["E1M7"];
    expected.extend(MAP_LUMPS);
    assert_eq!(names, expected);

    let indexes: Vec<_> = wad.map("E1M7").map(|l| l.index).collect();
    assert!(
        indexes.windows(2).all(|w| w[1] == w[0] + 1),
        "the map's lumps are not contiguous: {indexes:?}"
    );
}

/// `(lump, record size, record count)` for E1M7. Every size divides
/// exactly, which is what says the record sizes are right.
const E1M7_RECORDS: [(&str, usize, usize); 8] = [
    ("THINGS", 10, 358),
    ("LINEDEFS", 14, 958),
    ("SIDEDEFS", 30, 1223),
    ("VERTEXES", 4, 896),
    ("SEGS", 12, 1371),
    ("SSECTORS", 4, 467),
    ("NODES", 28, 466),
    ("SECTORS", 26, 170),
];

#[test]
fn e1m7_lump_sizes_divide_into_the_expected_record_counts() {
    let bytes = doom1();
    let wad = Wad::parse(&bytes).unwrap();
    for (name, record, count) in E1M7_RECORDS {
        let lump = wad.map_lump("E1M7", name).unwrap();
        assert_eq!(
            lump.bytes.len() % record,
            0,
            "{name} is {} bytes, not a multiple of {record}",
            lump.bytes.len()
        );
        assert_eq!(lump.bytes.len() / record, count, "{name} record count");
    }
    assert_eq!(wad.map_lump("E1M7", "REJECT").unwrap().bytes.len(), 3613);
    assert_eq!(wad.map_lump("E1M7", "BLOCKMAP").unwrap().bytes.len(), 8846);
}

/// `P_LoadReject` treats REJECT as `numsectors * numsectors` bits packed
/// end to end, with no padding per row, and pads the lump itself out to
/// that length when it is short. E1M7's is exactly long enough.
#[test]
fn the_reject_matrix_covers_every_sector_pair() {
    let bytes = doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let sectors = wad.map_lump("E1M7", "SECTORS").unwrap().bytes.len() / 26;
    let reject = wad.map_lump("E1M7", "REJECT").unwrap().bytes.len();
    assert_eq!(reject, (sectors * sectors).div_ceil(8));
}

#[test]
fn demo3_carries_a_thirteen_byte_header_and_2134_ticcmds() {
    let bytes = doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let demo = wad.find("DEMO3").unwrap().bytes;
    assert_eq!(demo.len(), 8550);
    // version, skill, episode, map: the demo runs E1M7 on skill 2.
    assert_eq!(demo[..4], [109, 2, 1, 7]);
    assert_eq!(*demo.last().unwrap(), 0x80, "demo terminator");
    let body = &demo[13..demo.len() - 1];
    assert_eq!(body.len() % 4, 0);
    assert_eq!(body.len() / 4, 2134);
    // forwardmove, sidemove, angleturn, buttons of the first tic command.
    assert_eq!(body[..4], [50, 0, 242, 0]);
}

#[test]
fn the_asset_lumps_the_level_load_reads_are_all_present() {
    let bytes = doom1();
    let wad = Wad::parse(&bytes).unwrap();
    for name in [
        "PLAYPAL", "COLORMAP", "PNAMES", "TEXTURE1", "S_START", "S_END", "F1_START", "F1_END",
        "STBAR", "STTNUM0", "STCFN033",
    ] {
        assert!(wad.find(name).is_some(), "{name} is missing");
    }
    assert_eq!(wad.find("PLAYPAL").unwrap().bytes.len(), 14 * 768);
    assert_eq!(wad.find("COLORMAP").unwrap().bytes.len(), 34 * 256);
}
