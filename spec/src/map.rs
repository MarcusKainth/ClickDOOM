//! The memory map, and the MMIO register offsets inside it.

use serde::{Deserialize, Serialize};

/// RAM. The ROM image loads at this address; code, data, heap and stack all
/// live here.
pub const RAM_BASE: u32 = 0x8000_0000;
pub const RAM_SIZE: u32 = 24 * 1024 * 1024;

/// The device window. Word access only.
pub const MMIO_BASE: u32 = 0x1000_0000;
pub const MMIO_SIZE: u32 = 4 * 1024;

/// 320x200, 8bpp palette-indexed, row-major.
pub const FRAMEBUFFER_BASE: u32 = 0x1100_0000;
pub const FRAMEBUFFER_SIZE: u32 = 64_000;

/// 256 entries of RGB, three bytes each.
pub const PALETTE_BASE: u32 = 0x1101_0000;
pub const PALETTE_SIZE: u32 = 768;

/// Instructions per emulated millisecond. Time advances with retired
/// instructions and never with a host clock.
pub const IPMS_DEFAULT: u32 = 10_000;

/// Register offsets within the MMIO window.
pub mod mmio {
    /// Read: retired instructions divided by the elastic-time constant.
    pub const TICKS_MS: u32 = 0x00;
    /// Read: pop one key event, or 0 when the queue is empty.
    pub const KEYQ: u32 = 0x04;
    /// Write: stop the machine, with the written value as the exit code.
    pub const EXIT: u32 = 0x08;
    /// Write: append the low byte to the console.
    pub const PUTCHAR: u32 = 0x0C;
    /// Write: the framebuffer holds a complete frame, numbered by the value.
    pub const FRAME_COMMIT: u32 = 0x10;
}

/// A key event as `KEYQ` returns it.
pub const fn key_event(pressed: bool, doomkey: u8) -> u32 {
    ((pressed as u32) << 8) | doomkey as u32
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    Ram,
    Mmio,
    Framebuffer,
    Palette,
}

impl Region {
    pub const fn as_str(self) -> &'static str {
        match self {
            Region::Ram => "ram",
            Region::Mmio => "mmio",
            Region::Framebuffer => "framebuffer",
            Region::Palette => "palette",
        }
    }
}

/// Where each region sits. The sizes are settings rather than constants so a
/// binary other than this ROM can declare its own, and so a test can run a
/// small machine.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MemoryMap {
    pub ram_base: u32,
    pub ram_size: u32,
    pub mmio_base: u32,
    pub mmio_size: u32,
    pub framebuffer_base: u32,
    pub framebuffer_size: u32,
    pub palette_base: u32,
    pub palette_size: u32,
}

impl Default for MemoryMap {
    fn default() -> Self {
        Self::clickdoom()
    }
}

impl MemoryMap {
    /// The map this ROM is built against.
    pub const fn clickdoom() -> Self {
        Self {
            ram_base: RAM_BASE,
            ram_size: RAM_SIZE,
            mmio_base: MMIO_BASE,
            mmio_size: MMIO_SIZE,
            framebuffer_base: FRAMEBUFFER_BASE,
            framebuffer_size: FRAMEBUFFER_SIZE,
            palette_base: PALETTE_BASE,
            palette_size: PALETTE_SIZE,
        }
    }

    /// The same map with a different RAM size, for a smaller machine.
    pub const fn with_ram_size(mut self, ram_size: u32) -> Self {
        self.ram_size = ram_size;
        self
    }

    /// Which region holds `[addr, addr + width)`, and the offset within it.
    /// `None` when the access spans no region or straddles the end of one.
    pub fn classify(&self, addr: u32, width: u32) -> Option<(Region, u32)> {
        const REGIONS: [Region; 4] = [
            Region::Ram,
            Region::Mmio,
            Region::Framebuffer,
            Region::Palette,
        ];
        for region in REGIONS {
            let (base, size) = self.bounds(region);
            // Both sides widened to u64 so a base near the top of the address
            // space cannot wrap the addition into a false hit.
            let end = base as u64 + size as u64;
            if addr as u64 >= base as u64 && addr as u64 + width as u64 <= end {
                return Some((region, addr - base));
            }
        }
        None
    }

    pub const fn bounds(&self, region: Region) -> (u32, u32) {
        match region {
            Region::Ram => (self.ram_base, self.ram_size),
            Region::Mmio => (self.mmio_base, self.mmio_size),
            Region::Framebuffer => (self.framebuffer_base, self.framebuffer_size),
            Region::Palette => (self.palette_base, self.palette_size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_event_packs_pressed_above_the_keycode() {
        assert_eq!(key_event(true, 0x41), 0x141);
        assert_eq!(key_event(false, 0x41), 0x041);
    }

    #[test]
    fn classify_finds_each_region() {
        let map = MemoryMap::clickdoom();
        assert_eq!(map.classify(RAM_BASE, 4), Some((Region::Ram, 0)));
        assert_eq!(map.classify(MMIO_BASE + 8, 4), Some((Region::Mmio, 8)));
        assert_eq!(
            map.classify(FRAMEBUFFER_BASE + 4, 4),
            Some((Region::Framebuffer, 4))
        );
        assert_eq!(map.classify(PALETTE_BASE, 4), Some((Region::Palette, 0)));
    }

    #[test]
    fn classify_rejects_an_access_that_runs_past_a_region() {
        let map = MemoryMap::clickdoom();
        assert_eq!(map.classify(RAM_BASE + RAM_SIZE - 3, 4), None);
        assert_eq!(map.classify(FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE, 1), None);
    }

    #[test]
    fn classify_rejects_the_gaps() {
        let map = MemoryMap::clickdoom();
        assert_eq!(map.classify(0, 4), None);
        assert_eq!(map.classify(0xFFFF_FFF0, 4), None);
        assert_eq!(map.classify(RAM_BASE + RAM_SIZE, 4), None);
        assert_eq!(map.classify(MMIO_BASE + MMIO_SIZE, 4), None);
    }
}
