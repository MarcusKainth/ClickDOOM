//! Which map a lump belongs to.
//!
//! A map is a zero-length marker lump followed by the map's own lumps, and
//! the map lump names repeat once per map. The marker is the only thing
//! that tells `SEGS` for `E1M7` from `SEGS` for `E1M1`, so every lump
//! carries the name of the marker enclosing it.

/// The map lump names, in the order a map stores them.
pub const MAP_LUMPS: [&str; 10] = [
    "THINGS", "LINEDEFS", "SIDEDEFS", "VERTEXES", "SEGS", "SSECTORS", "NODES", "SECTORS", "REJECT",
    "BLOCKMAP",
];

/// True for a map marker name: `ExMy` or `MAPxx`.
pub fn is_marker(name: &str) -> bool {
    let b = name.as_bytes();
    match b.len() {
        4 => b[0] == b'E' && b[1].is_ascii_digit() && b[2] == b'M' && b[3].is_ascii_digit(),
        5 => &b[..3] == b"MAP" && b[3].is_ascii_digit() && b[4].is_ascii_digit(),
        _ => false,
    }
}

/// True for a lump name a map owns.
pub fn is_map_lump(name: &str) -> bool {
    MAP_LUMPS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_exmy_or_mapxx() {
        assert!(is_marker("E1M7"));
        assert!(is_marker("MAP07"));
        assert!(!is_marker("E1M"));
        assert!(!is_marker("EXMY"));
        assert!(!is_marker("MAP7"));
        assert!(!is_marker("THINGS"));
    }

    #[test]
    fn map_lumps_are_the_ten_names() {
        assert!(is_map_lump("BLOCKMAP"));
        assert!(is_map_lump("REJECT"));
        assert!(!is_map_lump("PNAMES"));
    }
}
