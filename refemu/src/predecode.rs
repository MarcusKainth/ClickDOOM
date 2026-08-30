//! Decoded instructions, kept for the read-only text region.
//!
//! The cache covers exactly that region and nothing else. A store into it is a
//! self-modify halt, so the region cannot change under the cache and there is
//! no invalidation to get wrong and nothing for the store path to check.
//! Outside the region, and when no region is declared, instructions decode on
//! fetch. That is the path the riscv-tests fixtures take.
//!
//! The cache is an acceleration and owes a proof: cached and uncached
//! execution agree instruction for instruction. A test runs the same program
//! both ways and compares the whole checkpoint trace.

use crate::decode::{Instruction, decode};
use crate::memory::Memory;

/// One cached instruction, with the word it came from.
///
/// The word is here because a halt record names the instruction that caused
/// it, and reading it back from memory on that path would cost a second
/// region dispatch for something the fetch already had.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Entry {
    pub insn: Instruction,
    pub word: u32,
}

pub struct DecodeCache {
    /// The first address covered, four-byte aligned.
    base: u32,
    entries: Box<[Entry]>,
}

impl DecodeCache {
    /// Decodes `[start, end)` up front.
    ///
    /// Returns nothing when the region is empty, is not four-byte aligned, or
    /// is not wholly inside RAM. Each of those leaves every fetch on the
    /// decode-on-read path, which is correct and slower.
    pub fn build(memory: &Memory, start: u32, end: u32) -> Option<Self> {
        let map = memory.map();
        if start >= end || !start.is_multiple_of(4) || !end.is_multiple_of(4) {
            return None;
        }
        let ram_end = map.ram_base as u64 + map.ram_size as u64;
        if (start as u64) < map.ram_base as u64 || end as u64 > ram_end {
            return None;
        }
        let ram = memory.ram();
        let count = ((end - start) / 4) as usize;
        let mut entries = Vec::with_capacity(count);
        for index in 0..count {
            let at = (start - map.ram_base) as usize + index * 4;
            let word = u32::from_le_bytes([ram[at], ram[at + 1], ram[at + 2], ram[at + 3]]);
            entries.push(Entry {
                insn: decode(word),
                word,
            });
        }
        Some(Self {
            base: start,
            entries: entries.into_boxed_slice(),
        })
    }

    /// The entry for `pc`, when the cache covers it.
    ///
    /// A program counter that is not four-byte aligned finds nothing here and
    /// takes the fetch path, which reports the misalignment.
    #[inline(always)]
    pub fn get(&self, pc: u32) -> Option<Entry> {
        let delta = pc.wrapping_sub(self.base);
        if !delta.is_multiple_of(4) {
            return None;
        }
        self.entries.get((delta / 4) as usize).copied()
    }

    pub const fn base(&self) -> u32 {
        self.base
    }

    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How much the cache costs to hold.
    pub const fn bytes(&self) -> usize {
        self.entries.len() * size_of::<Entry>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::{addi, program};
    use crate::mmio::Devices;
    use clickdoom_spec::{MemoryMap, RAM_BASE};

    fn memory_with(words: &[u32]) -> Memory {
        let map = MemoryMap::clickdoom().with_ram_size(4096);
        let mut memory = Memory::new(map, Devices::bytes(map.mmio_size));
        memory.load_image(&program(words), RAM_BASE).unwrap();
        memory
    }

    #[test]
    fn a_cache_decodes_the_region_it_is_given() {
        let memory = memory_with(&[addi(1, 0, 1), addi(2, 0, 2)]);
        let cache = DecodeCache::build(&memory, RAM_BASE, RAM_BASE + 8).unwrap();
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(RAM_BASE).unwrap().word, addi(1, 0, 1));
        assert_eq!(cache.get(RAM_BASE + 4).unwrap().word, addi(2, 0, 2));
        assert_eq!(cache.get(RAM_BASE).unwrap().insn, decode(addi(1, 0, 1)));
    }

    #[test]
    fn an_address_outside_the_region_finds_nothing() {
        let memory = memory_with(&[addi(1, 0, 1), addi(2, 0, 2)]);
        let cache = DecodeCache::build(&memory, RAM_BASE, RAM_BASE + 8).unwrap();
        assert!(cache.get(RAM_BASE + 8).is_none());
        assert!(cache.get(RAM_BASE - 4).is_none());
        assert!(cache.get(0).is_none());
    }

    #[test]
    fn a_misaligned_address_finds_nothing_rather_than_the_word_below_it() {
        let memory = memory_with(&[addi(1, 0, 1), addi(2, 0, 2)]);
        let cache = DecodeCache::build(&memory, RAM_BASE, RAM_BASE + 8).unwrap();
        for offset in 1..4 {
            assert!(
                cache.get(RAM_BASE + offset).is_none(),
                "offset {offset} found an entry"
            );
        }
    }

    #[test]
    fn a_region_the_cache_cannot_cover_leaves_every_fetch_on_the_slow_path() {
        let memory = memory_with(&[addi(1, 0, 1)]);
        // Empty, misaligned, and running past the end of RAM.
        assert!(DecodeCache::build(&memory, RAM_BASE, RAM_BASE).is_none());
        assert!(DecodeCache::build(&memory, RAM_BASE + 1, RAM_BASE + 9).is_none());
        assert!(DecodeCache::build(&memory, RAM_BASE, RAM_BASE + 8192).is_none());
        assert!(DecodeCache::build(&memory, 0, RAM_BASE + 8).is_none());
    }

    #[test]
    fn an_entry_is_twelve_bytes() {
        assert_eq!(size_of::<Entry>(), 12);
    }
}
