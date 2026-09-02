//! Typed reads over the machine's RAM.
//!
//! The probe reads the engine's own data structures, so it goes at the RAM
//! bytes directly rather than through `Memory::read`. A device read pops the
//! key queue, and an observation that changes the run is not an observation.
//!
//! Every read is bounds-checked. The addresses come from pointers the engine
//! wrote, so a corrupt one has to be an error naming the address rather than a
//! panic or a silently wrong number.

use super::ProbeError;

/// The RAM region, addressed the way the program addresses it.
pub struct Ram<'a> {
    base: u32,
    bytes: &'a [u8],
}

impl<'a> Ram<'a> {
    pub const fn new(base: u32, bytes: &'a [u8]) -> Self {
        Self { base, bytes }
    }

    /// Whether an address is inside RAM. A pointer the engine set to NULL is
    /// not, which is how a caller tells "no target" from "read this".
    pub fn holds(&self, addr: u32) -> bool {
        addr >= self.base && ((addr - self.base) as usize) < self.bytes.len()
    }

    fn slice(&self, addr: u32, len: u32, what: &'static str) -> Result<&[u8], ProbeError> {
        let at = addr.checked_sub(self.base).map(|at| at as usize);
        let range = at.and_then(|at| Some(at..at.checked_add(len as usize)?));
        match range {
            Some(range) if range.end <= self.bytes.len() => Ok(&self.bytes[range]),
            _ => Err(ProbeError::OutsideRam { addr, len, what }),
        }
    }

    pub fn u8(&self, addr: u32, what: &'static str) -> Result<u8, ProbeError> {
        Ok(self.slice(addr, 1, what)?[0])
    }

    pub fn i8(&self, addr: u32, what: &'static str) -> Result<i8, ProbeError> {
        Ok(self.u8(addr, what)? as i8)
    }

    pub fn u16(&self, addr: u32, what: &'static str) -> Result<u16, ProbeError> {
        let bytes = self.slice(addr, 2, what)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn i16(&self, addr: u32, what: &'static str) -> Result<i16, ProbeError> {
        Ok(self.u16(addr, what)? as i16)
    }

    pub fn u32(&self, addr: u32, what: &'static str) -> Result<u32, ProbeError> {
        let bytes = self.slice(addr, 4, what)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn i32(&self, addr: u32, what: &'static str) -> Result<i32, ProbeError> {
        Ok(self.u32(addr, what)? as i32)
    }

    /// A NUL-terminated string, stopping at `limit` bytes whether or not the
    /// engine terminated it.
    pub fn cstr(&self, addr: u32, limit: u32, what: &'static str) -> Result<&[u8], ProbeError> {
        if !self.holds(addr) {
            return Err(ProbeError::OutsideRam {
                addr,
                len: limit,
                what,
            });
        }
        let available = self
            .bytes
            .len()
            .saturating_sub(addr.saturating_sub(self.base) as usize);
        let len = limit.min(available as u32);
        let bytes = self.slice(addr, len, what)?;
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        Ok(&bytes[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u32 = 0x8000_0000;

    fn ram() -> Vec<u8> {
        let mut bytes = vec![0u8; 64];
        bytes[0..4].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
        bytes[4..6].copy_from_slice(&0xFFFEu16.to_le_bytes());
        bytes[8] = 0xFF;
        bytes[16..21].copy_from_slice(b"DOOM\0");
        bytes
    }

    #[test]
    fn a_word_reads_signed_and_unsigned_from_the_same_bytes() {
        let bytes = ram();
        let ram = Ram::new(BASE, &bytes);
        assert_eq!(ram.u32(BASE, "w").unwrap(), 0xFFFF_FFFE);
        assert_eq!(ram.i32(BASE, "w").unwrap(), -2);
        assert_eq!(ram.u16(BASE + 4, "h").unwrap(), 0xFFFE);
        assert_eq!(ram.i16(BASE + 4, "h").unwrap(), -2);
        assert_eq!(ram.u8(BASE + 8, "b").unwrap(), 0xFF);
        assert_eq!(ram.i8(BASE + 8, "b").unwrap(), -1);
    }

    #[test]
    fn an_address_outside_ram_is_an_error_naming_it() {
        let bytes = ram();
        let ram = Ram::new(BASE, &bytes);
        assert!(!ram.holds(0));
        assert!(!ram.holds(BASE + 64));
        assert!(ram.holds(BASE));
        assert!(ram.holds(BASE + 63));
        for addr in [0, BASE - 4, BASE + 61, BASE + 64, 0xFFFF_FFFC] {
            assert!(
                matches!(ram.u32(addr, "w"), Err(ProbeError::OutsideRam { .. })),
                "at {addr:#010x}"
            );
        }
    }

    #[test]
    fn a_string_stops_at_its_terminator_and_at_the_limit() {
        let bytes = ram();
        let ram = Ram::new(BASE, &bytes);
        assert_eq!(ram.cstr(BASE + 16, 64, "s").unwrap(), b"DOOM");
        assert_eq!(ram.cstr(BASE + 16, 2, "s").unwrap(), b"DO");
        // An unterminated run up to the end of RAM stops there rather than
        // reading past it.
        assert_eq!(ram.cstr(BASE + 62, 64, "s").unwrap(), b"");
        assert!(ram.cstr(BASE + 64, 8, "s").is_err());
    }
}
