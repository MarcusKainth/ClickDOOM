//! Flat physical memory: four regions, byte-addressable, little-endian.
//!
//! Anything outside them is a fatal bad-address halt. Within RAM, the text
//! region is read-only, and a store there is a self-modify halt rather than a
//! write that lands.
//!
//! Alignment is checked before region membership, so a misaligned access to an
//! address in no region reports misalignment rather than a bad address. The
//! two engines have to agree on which, and the order is what decides it.

use clickdoom_spec::map::{MemoryMap, Region};

use crate::mmio::{Devices, MmioExit};

/// What a memory access can raise. The interpreter turns each into a halt.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MemFault {
    /// Outside every declared region.
    BadAddr { addr: u32 },
    /// A half or word access not aligned to its own width.
    Misaligned { addr: u32, width: u32 },
    /// A store into the read-only text region.
    SelfModify { addr: u32 },
    /// The program wrote the exit register. Not a fault.
    Exit { code: u32 },
}

impl From<MmioExit> for MemFault {
    fn from(exit: MmioExit) -> Self {
        MemFault::Exit { code: exit.code }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LoadError {
    #[error("load address {load_addr:#010x} is below the RAM base {ram_base:#010x}")]
    BelowRam { load_addr: u32, ram_base: u32 },
    #[error(
        "an image of {len} bytes at {load_addr:#010x} runs past the end of {ram_size}-byte RAM"
    )]
    TooLarge {
        load_addr: u32,
        len: usize,
        ram_size: u32,
    },
}

pub struct Memory {
    map: MemoryMap,
    ram: Box<[u8]>,
    framebuffer: Box<[u8]>,
    palette: Box<[u8]>,
    devices: Devices,
    /// The read-only region, half open, absolute addresses. Both bounds or
    /// neither: a half-declared region protects nothing and would differ from
    /// an undeclared one only by accident.
    text: Option<(u32, u32)>,
}

impl Memory {
    pub fn new(map: MemoryMap, devices: Devices) -> Self {
        Self {
            ram: vec![0; map.ram_size as usize].into_boxed_slice(),
            framebuffer: vec![0; map.framebuffer_size as usize].into_boxed_slice(),
            palette: vec![0; map.palette_size as usize].into_boxed_slice(),
            devices,
            text: None,
            map,
        }
    }

    pub const fn map(&self) -> &MemoryMap {
        &self.map
    }

    /// The RAM region in address order, which is the byte sequence the RAM
    /// hash covers with no re-serialisation.
    pub fn ram(&self) -> &[u8] {
        &self.ram
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }

    pub fn palette(&self) -> &[u8] {
        &self.palette
    }

    pub fn devices(&self) -> &Devices {
        &self.devices
    }

    pub fn devices_mut(&mut self) -> &mut Devices {
        &mut self.devices
    }

    pub const fn text_region(&self) -> Option<(u32, u32)> {
        self.text
    }

    pub const fn set_text_region(&mut self, region: Option<(u32, u32)>) {
        self.text = region;
    }

    /// Loads an image at `load_addr`.
    ///
    /// This is not a store. It bypasses the text check, because a program's
    /// own code arriving in RAM is what declares that region rather than a
    /// violation of it.
    pub fn load_image(&mut self, data: &[u8], load_addr: u32) -> Result<(), LoadError> {
        if load_addr < self.map.ram_base {
            return Err(LoadError::BelowRam {
                load_addr,
                ram_base: self.map.ram_base,
            });
        }
        let at = (load_addr - self.map.ram_base) as usize;
        let end = at
            .checked_add(data.len())
            .filter(|end| *end <= self.ram.len())
            .ok_or(LoadError::TooLarge {
                load_addr,
                len: data.len(),
                ram_size: self.map.ram_size,
            })?;
        self.ram[at..end].copy_from_slice(data);
        Ok(())
    }

    const fn check_align(addr: u32, width: u32) -> Result<(), MemFault> {
        if width > 1 && !addr.is_multiple_of(width) {
            return Err(MemFault::Misaligned { addr, width });
        }
        Ok(())
    }

    /// Whether `[addr, addr + width)` touches the read-only region at all.
    fn in_text(&self, addr: u32, width: u32) -> bool {
        match self.text {
            None => false,
            Some((start, end)) => !(addr as u64 + width as u64 <= start as u64 || addr >= end),
        }
    }

    /// Reads `width` bytes little-endian. Width is 1, 2 or 4.
    pub fn read(&mut self, addr: u32, width: u32, icount: u64) -> Result<u32, MemFault> {
        Self::check_align(addr, width)?;
        let (region, offset) = self
            .map
            .classify(addr, width)
            .ok_or(MemFault::BadAddr { addr })?;
        Ok(match region {
            Region::Ram => read_le(&self.ram, offset, width),
            Region::Mmio => self.devices.read(offset, width, icount),
            Region::Framebuffer => read_le(&self.framebuffer, offset, width),
            Region::Palette => read_le(&self.palette, offset, width),
        })
    }

    /// Writes the low `width` bytes of `value` little-endian.
    pub fn write(
        &mut self,
        addr: u32,
        width: u32,
        value: u32,
        icount: u64,
    ) -> Result<(), MemFault> {
        Self::check_align(addr, width)?;
        let (region, offset) = self
            .map
            .classify(addr, width)
            .ok_or(MemFault::BadAddr { addr })?;
        match region {
            Region::Ram => {
                if self.in_text(addr, width) {
                    return Err(MemFault::SelfModify { addr });
                }
                write_le(&mut self.ram, offset, width, value);
            }
            Region::Mmio => self.devices.write(offset, width, value, icount)?,
            // A narrower store into the pixel regions would be a
            // read-modify-write against a word nothing ever reads back, so
            // there is no correct value to write.
            Region::Framebuffer => {
                if width != 4 {
                    return Err(MemFault::BadAddr { addr });
                }
                write_le(&mut self.framebuffer, offset, width, value);
            }
            Region::Palette => {
                if width != 4 {
                    return Err(MemFault::BadAddr { addr });
                }
                write_le(&mut self.palette, offset, width, value);
            }
        }
        Ok(())
    }
}

fn read_le(bytes: &[u8], offset: u32, width: u32) -> u32 {
    let at = offset as usize;
    match width {
        1 => bytes[at] as u32,
        2 => u16::from_le_bytes([bytes[at], bytes[at + 1]]) as u32,
        4 => u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]),
        _ => unreachable!("width is 1, 2 or 4"),
    }
}

fn write_le(bytes: &mut [u8], offset: u32, width: u32, value: u32) {
    let at = offset as usize;
    let le = value.to_le_bytes();
    bytes[at..at + width as usize].copy_from_slice(&le[..width as usize]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickdoom_spec::map::mmio;
    use clickdoom_spec::{
        FRAMEBUFFER_BASE, FRAMEBUFFER_SIZE, IPMS_DEFAULT, MMIO_BASE, MMIO_SIZE, PALETTE_BASE,
        PALETTE_SIZE, RAM_BASE, RAM_SIZE,
    };

    fn memory() -> Memory {
        Memory::new(MemoryMap::clickdoom(), Devices::registers(IPMS_DEFAULT))
    }

    fn inert() -> Memory {
        Memory::new(MemoryMap::clickdoom(), Devices::bytes(MMIO_SIZE))
    }

    #[test]
    fn ram_round_trips_at_every_width() {
        let mut m = memory();
        m.write(RAM_BASE, 4, 0xDEAD_BEEF, 0).unwrap();
        assert_eq!(m.read(RAM_BASE, 4, 0), Ok(0xDEAD_BEEF));
        assert_eq!(m.read(RAM_BASE, 1, 0), Ok(0xEF));
        assert_eq!(m.read(RAM_BASE + 2, 2, 0), Ok(0xDEAD));
        m.write(RAM_BASE + 8, 1, 0x1FF, 0).unwrap();
        assert_eq!(m.read(RAM_BASE + 8, 1, 0), Ok(0xFF));
        m.write(RAM_BASE + 12, 2, 0x1_2345, 0).unwrap();
        assert_eq!(m.read(RAM_BASE + 12, 2, 0), Ok(0x2345));
    }

    #[test]
    fn an_address_outside_every_region_is_a_bad_address() {
        let mut m = memory();
        for addr in [0, 0xFFFF_FFF0, RAM_BASE - 4, MMIO_BASE - 4] {
            assert_eq!(m.read(addr, 4, 0), Err(MemFault::BadAddr { addr }));
            assert_eq!(m.write(addr, 4, 1, 0), Err(MemFault::BadAddr { addr }));
        }
    }

    #[test]
    fn just_past_ram_is_a_bad_address() {
        let mut m = memory();
        let addr = RAM_BASE + RAM_SIZE;
        assert_eq!(m.read(addr, 4, 0), Err(MemFault::BadAddr { addr }));
        // The last word inside is fine, and a word straddling the end is not.
        assert_eq!(m.read(RAM_BASE + RAM_SIZE - 4, 4, 0), Ok(0));
        let straddling = RAM_BASE + RAM_SIZE - 2;
        assert_eq!(
            m.read(straddling, 4, 0),
            Err(MemFault::Misaligned {
                addr: straddling,
                width: 4
            })
        );
    }

    #[test]
    fn every_declared_region_is_reachable() {
        let mut m = memory();
        for base in [RAM_BASE, MMIO_BASE, FRAMEBUFFER_BASE, PALETTE_BASE] {
            assert!(m.read(base, 4, 0).is_ok(), "{base:#010x} is not readable");
        }
    }

    #[test]
    fn alignment_is_decided_before_region_membership() {
        let mut m = memory();
        // In no region and misaligned. Misalignment is the answer, because it
        // is checked first.
        assert_eq!(
            m.read(0x0000_0002, 4, 0),
            Err(MemFault::Misaligned { addr: 2, width: 4 })
        );
        assert_eq!(
            m.write(0x0000_0001, 2, 0, 0),
            Err(MemFault::Misaligned { addr: 1, width: 2 })
        );
    }

    #[test]
    fn a_misaligned_half_or_word_access_faults() {
        let mut m = memory();
        assert_eq!(
            m.read(RAM_BASE + 1, 4, 0),
            Err(MemFault::Misaligned {
                addr: RAM_BASE + 1,
                width: 4
            })
        );
        assert_eq!(
            m.write(RAM_BASE + 1, 2, 0, 0),
            Err(MemFault::Misaligned {
                addr: RAM_BASE + 1,
                width: 2
            })
        );
    }

    #[test]
    fn a_byte_access_is_never_misaligned() {
        let mut m = memory();
        for offset in 0..4 {
            assert!(m.read(RAM_BASE + offset, 1, 0).is_ok());
            assert!(m.write(RAM_BASE + offset, 1, 0xAB, 0).is_ok());
        }
    }

    #[test]
    fn a_sub_word_store_to_the_pixel_regions_is_a_bad_address() {
        let mut m = memory();
        for base in [FRAMEBUFFER_BASE, PALETTE_BASE] {
            for width in [1, 2] {
                assert_eq!(
                    m.write(base, width, 0xFF, 0),
                    Err(MemFault::BadAddr { addr: base }),
                    "a {width}-byte store at {base:#010x} was allowed"
                );
            }
            // A word store still lands, and reads stay open at any width.
            m.write(base, 4, 0x0403_0201, 0).unwrap();
            assert_eq!(m.read(base, 4, 0), Ok(0x0403_0201));
            assert_eq!(m.read(base, 1, 0), Ok(0x01));
            assert_eq!(m.read(base + 2, 2, 0), Ok(0x0403));
        }
    }

    #[test]
    fn the_pixel_regions_are_exactly_as_large_as_declared() {
        let mut m = memory();
        assert_eq!(m.framebuffer().len(), FRAMEBUFFER_SIZE as usize);
        assert_eq!(m.palette().len(), PALETTE_SIZE as usize);
        assert!(
            m.write(FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE - 4, 4, 1, 0)
                .is_ok()
        );
        let past = FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE;
        assert_eq!(
            m.write(past, 4, 1, 0),
            Err(MemFault::BadAddr { addr: past })
        );
        assert!(m.write(PALETTE_BASE + PALETTE_SIZE - 4, 4, 1, 0).is_ok());
        let past = PALETTE_BASE + PALETTE_SIZE;
        assert_eq!(
            m.write(past, 4, 1, 0),
            Err(MemFault::BadAddr { addr: past })
        );
    }

    #[test]
    fn a_store_into_the_text_region_is_a_self_modify() {
        let mut m = memory();
        m.set_text_region(Some((RAM_BASE, RAM_BASE + 0x100)));
        assert_eq!(
            m.write(RAM_BASE, 4, 1, 0),
            Err(MemFault::SelfModify { addr: RAM_BASE })
        );
        // Reads are unaffected.
        assert_eq!(m.read(RAM_BASE, 4, 0), Ok(0));
    }

    #[test]
    fn the_text_region_end_is_exclusive() {
        let mut m = memory();
        let end = RAM_BASE + 0x100;
        m.set_text_region(Some((RAM_BASE, end)));
        assert_eq!(
            m.write(end - 4, 4, 1, 0),
            Err(MemFault::SelfModify { addr: end - 4 })
        );
        assert!(m.write(end, 4, 1, 0).is_ok());
    }

    #[test]
    fn a_store_overlapping_the_text_region_at_all_is_a_self_modify() {
        let mut m = memory();
        let start = RAM_BASE + 0x100;
        m.set_text_region(Some((start, start + 0x100)));
        // The word before the region ends exactly at its start, so it lands.
        assert!(m.write(start - 4, 4, 1, 0).is_ok());
        // A byte inside the first word of the region does not.
        assert_eq!(
            m.write(start + 3, 1, 1, 0),
            Err(MemFault::SelfModify { addr: start + 3 })
        );
    }

    #[test]
    fn no_declared_text_region_leaves_every_store_landing() {
        let mut m = memory();
        assert_eq!(m.text_region(), None);
        assert!(m.write(RAM_BASE, 4, 0xABCD, 0).is_ok());
        assert_eq!(m.read(RAM_BASE, 4, 0), Ok(0xABCD));
    }

    #[test]
    fn writing_the_exit_register_reports_the_written_code() {
        let mut m = memory();
        assert_eq!(
            m.write(MMIO_BASE + mmio::EXIT, 4, 0xFFFF_FFFF, 0),
            Err(MemFault::Exit { code: 0xFFFF_FFFF })
        );
    }

    #[test]
    fn the_tick_register_reads_from_the_count_it_is_given() {
        let mut m = Memory::new(MemoryMap::clickdoom(), Devices::registers(10));
        assert_eq!(m.read(MMIO_BASE + mmio::TICKS_MS, 4, 25), Ok(2));
    }

    #[test]
    fn the_inert_device_window_behaves_like_memory() {
        let mut m = inert();
        // The exit register does not stop a machine wired this way.
        assert!(m.write(MMIO_BASE + mmio::EXIT, 4, 7, 0).is_ok());
        assert_eq!(m.read(MMIO_BASE + mmio::EXIT, 4, 0), Ok(7));
        assert_eq!(m.read(MMIO_BASE + mmio::TICKS_MS, 4, 1_000_000), Ok(0));
    }

    #[test]
    fn an_image_lands_verbatim_and_is_not_a_store() {
        let mut m = memory();
        m.set_text_region(Some((RAM_BASE, RAM_BASE + 0x100)));
        m.load_image(&[1, 2, 3, 4], RAM_BASE).unwrap();
        assert_eq!(m.read(RAM_BASE, 4, 0), Ok(0x0403_0201));
    }

    #[test]
    fn an_image_that_does_not_fit_is_refused_rather_than_growing_ram() {
        let map = MemoryMap::clickdoom().with_ram_size(64);
        let mut m = Memory::new(map, Devices::registers(IPMS_DEFAULT));
        assert_eq!(
            m.load_image(&[0; 65], RAM_BASE),
            Err(LoadError::TooLarge {
                load_addr: RAM_BASE,
                len: 65,
                ram_size: 64
            })
        );
        assert_eq!(
            m.load_image(&[0; 8], RAM_BASE + 60),
            Err(LoadError::TooLarge {
                load_addr: RAM_BASE + 60,
                len: 8,
                ram_size: 64
            })
        );
        assert!(m.load_image(&[0; 64], RAM_BASE).is_ok());
    }

    #[test]
    fn an_image_below_the_ram_base_is_refused() {
        let mut m = memory();
        assert_eq!(
            m.load_image(&[1], RAM_BASE - 1),
            Err(LoadError::BelowRam {
                load_addr: RAM_BASE - 1,
                ram_base: RAM_BASE
            })
        );
    }

    #[test]
    fn ram_is_the_byte_sequence_the_hash_covers() {
        let mut m = memory();
        assert_eq!(m.ram().len(), RAM_SIZE as usize);
        m.write(RAM_BASE + 4, 4, 0x0403_0201, 0).unwrap();
        assert_eq!(&m.ram()[4..8], &[0x01, 0x02, 0x03, 0x04]);
    }
}
