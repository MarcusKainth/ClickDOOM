//! Comparing a rendered frame against the reference emulator's own.
//!
//! The reference framebuffer arrives as 64,000 bytes from a `refemu`
//! frame dump and is loaded into a table beside `native_frames`. Everything
//! that reads the two and reports where they differ is SQL text built here.

/// The first pixel where `native_frames.fb` for `frame` differs from the
/// reference in `{db}.ref_frames`, and how many differ in all.
///
/// A row comes back whatever the answer: `differing` is 0 when the frame
/// matches, and the coordinates are then meaningless. `region` restricts the
/// comparison to part of the screen, so a caller can ask about the view
/// without the status bar.
pub fn first_difference(db: &str, region: Region) -> String {
    let rows = region.rows();
    format!(
        "WITH \
         assumeNotNull((SELECT fb FROM {db}.native_frames WHERE frame = {{frame:UInt32}})) AS mine, \
         assumeNotNull((SELECT fb FROM {db}.ref_frames WHERE frame = {{frame:UInt32}})) AS reference, \
         arrayFilter(i -> substring(mine, i, 1) != substring(reference, i, 1), {rows}) AS bad \
         SELECT \
         toUInt64(length(bad)) AS differing, \
         toInt32(if(empty(bad), -1, (bad[1] - 1) % 320)) AS x, \
         toInt32(if(empty(bad), -1, intDiv(bad[1] - 1, 320))) AS y, \
         toInt32(if(empty(bad), -1, reinterpretAsUInt8(substring(mine, bad[1], 1)))) AS ours, \
         toInt32(if(empty(bad), -1, reinterpretAsUInt8(substring(reference, bad[1], 1)))) AS theirs"
    )
}

/// Which pixels a comparison looks at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Region {
    /// The whole 320x200 framebuffer.
    Screen,
    /// The 320x168 view the renderer draws into.
    View,
    /// The 320x32 status bar under it.
    StatusBar,
}

impl Region {
    /// The one-based framebuffer offsets the region covers.
    fn rows(self) -> &'static str {
        match self {
            Region::Screen => "range(1, 64001)",
            Region::View => "range(1, 53761)",
            Region::StatusBar => "range(53761, 64001)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_region_covers_the_pixels_it_names() {
        assert!(first_difference("nat", Region::View).contains("range(1, 53761)"));
        assert!(first_difference("nat", Region::StatusBar).contains("range(53761, 64001)"));
    }

    #[test]
    fn the_query_names_the_database_on_both_sides() {
        let sql = first_difference("nat", Region::Screen);
        assert!(sql.contains("nat.native_frames"));
        assert!(sql.contains("nat.ref_frames"));
    }
}
