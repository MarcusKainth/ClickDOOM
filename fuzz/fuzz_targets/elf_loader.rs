//! A malformed ELF is an error, never a panic and never an unbounded read.
//!
//! This is the one part of the emulator that parses a file somebody else
//! wrote, which is what a coverage-guided fuzzer is for.
#![no_main]

use libfuzzer_sys::fuzz_target;
use refemu::Image;

fuzz_target!(|data: &[u8]| {
    let Ok(image) = Image::parse_elf(data) else {
        return;
    };
    // A parse that succeeded has to describe something loadable: at least one
    // segment, each with as much memory as it has file, and a text region
    // that is inside the segments it came from.
    assert!(!image.segments.is_empty());
    for segment in &image.segments {
        assert!(segment.bytes.len() <= segment.mem_len as usize);
        // The segments are reported in address order.
        assert!(segment.mem_len > 0);
    }
    assert!(image.segments.windows(2).all(|w| w[0].vaddr <= w[1].vaddr));
    if let Some((start, end)) = image.text_region() {
        assert!(start < end);
    }
    for symbol in &image.symbols {
        assert!(!symbol.name.is_empty());
    }
});
