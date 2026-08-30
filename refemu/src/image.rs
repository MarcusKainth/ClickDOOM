//! Reading a program image.
//!
//! A flat binary is bytes at an address the caller names. An ELF says where
//! its own pieces go, which is what lets the emulator run a binary built
//! without this project's linker script.
//!
//! Everything here parses input the emulator did not produce, so every field
//! is bounds-checked against the file it came from and a malformed file is an
//! error rather than a panic.

use std::fmt;

use clickdoom_spec::RAM_BASE;

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS32: u8 = 1;
const ELFDATA2LSB: u8 = 1;
const EM_RISCV: u16 = 243;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const STT_FUNC: u8 = 2;

const EHDR_SIZE: usize = 52;
const PHDR_SIZE: usize = 32;
const SHDR_SIZE: usize = 40;
const SYM_SIZE: usize = 16;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    #[error("truncated at byte {at}, reading {what}")]
    Truncated { at: usize, what: &'static str },
    #[error("not an ELF file")]
    NotElf,
    #[error("not a 32-bit little-endian ELF")]
    WrongClass,
    #[error("machine is {0}, not RISC-V")]
    WrongMachine(u16),
    #[error("no loadable segment")]
    NoSegments,
    #[error("segment at {vaddr:#010x} has {filesz} bytes of file for {memsz} bytes of memory")]
    SegmentTooLong { vaddr: u32, filesz: u32, memsz: u32 },
}

/// One piece of a program, and where it goes.
#[derive(Clone, PartialEq, Eq)]
pub struct Segment {
    pub vaddr: u32,
    /// The bytes from the file. Shorter than `mem_len` when the segment has a
    /// zero-filled tail.
    pub bytes: Vec<u8>,
    pub mem_len: u32,
    pub executable: bool,
}

impl fmt::Debug for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Segment")
            .field("vaddr", &format_args!("{:#010x}", self.vaddr))
            .field("file_len", &self.bytes.len())
            .field("mem_len", &self.mem_len)
            .field("executable", &self.executable)
            .finish()
    }
}

/// A program ready to load.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Image {
    pub entry: u32,
    pub segments: Vec<Segment>,
    /// Every function symbol, sorted by address. Empty for a flat binary.
    pub symbols: Vec<Symbol>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Symbol {
    pub addr: u32,
    pub size: u32,
    pub name: String,
}

impl Image {
    /// A flat binary: the whole file at one address.
    pub fn flat(bytes: Vec<u8>, load_addr: u32) -> Self {
        let mem_len = bytes.len() as u32;
        Self {
            entry: load_addr,
            segments: vec![Segment {
                vaddr: load_addr,
                bytes,
                mem_len,
                executable: true,
            }],
            symbols: Vec::new(),
        }
    }

    /// Whether a file looks like an ELF, which is how `auto` picks.
    pub fn looks_like_elf(bytes: &[u8]) -> bool {
        bytes.len() >= 4 && bytes[..4] == ELF_MAGIC
    }

    /// The bounds of the executable segments, which is the region a store
    /// must not reach. `None` when the image declares none.
    pub fn text_region(&self) -> Option<(u32, u32)> {
        let mut bounds: Option<(u32, u32)> = None;
        for segment in self.segments.iter().filter(|s| s.executable) {
            let end = segment.vaddr.saturating_add(segment.mem_len);
            bounds = Some(match bounds {
                None => (segment.vaddr, end),
                Some((lo, hi)) => (lo.min(segment.vaddr), hi.max(end)),
            });
        }
        bounds
    }

    /// Parses an ELF, taking its loadable segments and function symbols.
    pub fn parse_elf(bytes: &[u8]) -> Result<Self, ImageError> {
        let read = Reader(bytes);
        if bytes.len() < EHDR_SIZE {
            return Err(ImageError::Truncated {
                at: bytes.len(),
                what: "the file header",
            });
        }
        if bytes[..4] != ELF_MAGIC {
            return Err(ImageError::NotElf);
        }
        if bytes[4] != ELFCLASS32 || bytes[5] != ELFDATA2LSB {
            return Err(ImageError::WrongClass);
        }
        let machine = read.u16(18, "the machine")?;
        if machine != EM_RISCV {
            return Err(ImageError::WrongMachine(machine));
        }

        let entry = read.u32(24, "the entry point")?;
        let phoff = read.u32(28, "the segment table offset")? as usize;
        let shoff = read.u32(32, "the section table offset")? as usize;
        let phentsize = read.u16(42, "the segment entry size")? as usize;
        let phnum = read.u16(44, "the segment count")? as usize;
        let shentsize = read.u16(46, "the section entry size")? as usize;
        let shnum = read.u16(48, "the section count")? as usize;

        let mut segments = Vec::new();
        if phnum > 0 && phentsize >= PHDR_SIZE {
            for index in 0..phnum {
                let at = phoff + index * phentsize;
                if read.u32(at, "a segment header")? != PT_LOAD {
                    continue;
                }
                let offset = read.u32(at + 4, "a segment offset")? as usize;
                let vaddr = read.u32(at + 8, "a segment address")?;
                let filesz = read.u32(at + 16, "a segment file size")?;
                let memsz = read.u32(at + 20, "a segment memory size")?;
                let flags = read.u32(at + 24, "a segment's flags")?;
                if filesz > memsz {
                    return Err(ImageError::SegmentTooLong {
                        vaddr,
                        filesz,
                        memsz,
                    });
                }
                if memsz == 0 {
                    continue;
                }
                let end = offset
                    .checked_add(filesz as usize)
                    .filter(|end| *end <= bytes.len())
                    .ok_or(ImageError::Truncated {
                        at: offset,
                        what: "a segment's contents",
                    })?;
                segments.push(Segment {
                    vaddr,
                    bytes: bytes[offset..end].to_vec(),
                    mem_len: memsz,
                    executable: flags & PF_X != 0,
                });
            }
        }
        if segments.is_empty() {
            return Err(ImageError::NoSegments);
        }
        segments.sort_by_key(|s| s.vaddr);

        let symbols = if shnum > 0 && shentsize >= SHDR_SIZE {
            read_symbols(&read, shoff, shentsize, shnum)?
        } else {
            Vec::new()
        };

        Ok(Self {
            entry,
            segments,
            symbols,
        })
    }
}

fn read_symbols(
    read: &Reader<'_>,
    shoff: usize,
    shentsize: usize,
    shnum: usize,
) -> Result<Vec<Symbol>, ImageError> {
    let mut symbols = Vec::new();
    for index in 0..shnum {
        let at = shoff + index * shentsize;
        if read.u32(at + 4, "a section type")? != SHT_SYMTAB {
            continue;
        }
        let offset = read.u32(at + 16, "a symbol table offset")? as usize;
        let size = read.u32(at + 20, "a symbol table size")? as usize;
        let strtab_index = read.u32(at + 24, "a string table index")? as usize;
        if strtab_index >= shnum {
            continue;
        }
        let str_at = shoff + strtab_index * shentsize;
        let str_off = read.u32(str_at + 16, "a string table offset")? as usize;
        let str_size = read.u32(str_at + 20, "a string table size")? as usize;

        for entry in 0..size / SYM_SIZE {
            let sym = offset + entry * SYM_SIZE;
            let info = read.u8(sym + 12, "a symbol's kind")?;
            if info & 0xF != STT_FUNC {
                continue;
            }
            let name_at = read.u32(sym, "a symbol's name")? as usize;
            let addr = read.u32(sym + 4, "a symbol's address")?;
            let sym_size = read.u32(sym + 8, "a symbol's size")?;
            let name = read.string(str_off, str_size, name_at)?;
            if name.is_empty() {
                continue;
            }
            symbols.push(Symbol {
                addr,
                size: sym_size,
                name,
            });
        }
    }
    symbols.sort_by(|a, b| a.addr.cmp(&b.addr).then_with(|| a.name.cmp(&b.name)));
    Ok(symbols)
}

/// Bounds-checked reads over a file the emulator did not write.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn slice(&self, at: usize, len: usize, what: &'static str) -> Result<&[u8], ImageError> {
        self.0
            .get(
                at..at
                    .checked_add(len)
                    .ok_or(ImageError::Truncated { at, what })?,
            )
            .ok_or(ImageError::Truncated { at, what })
    }

    fn u8(&self, at: usize, what: &'static str) -> Result<u8, ImageError> {
        Ok(self.slice(at, 1, what)?[0])
    }

    fn u16(&self, at: usize, what: &'static str) -> Result<u16, ImageError> {
        let bytes = self.slice(at, 2, what)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&self, at: usize, what: &'static str) -> Result<u32, ImageError> {
        let bytes = self.slice(at, 4, what)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A name from a string table, stopping at the table's end whether or not
    /// the file terminates it.
    fn string(&self, table: usize, size: usize, at: usize) -> Result<String, ImageError> {
        if at >= size {
            return Ok(String::new());
        }
        let available = self.slice(table, size, "a string table")?;
        let tail = &available[at..];
        let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
        Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
    }
}

/// Reads a file as whichever form it is.
pub fn read_image(bytes: Vec<u8>, load_addr: Option<u32>) -> Result<Image, ImageError> {
    if Image::looks_like_elf(&bytes) {
        Image::parse_elf(&bytes)
    } else {
        Ok(Image::flat(bytes, load_addr.unwrap_or(RAM_BASE)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an ELF with one loadable segment, so the reader is tested
    /// against bytes rather than against a file that happens to be present.
    fn elf_with(entry: u32, vaddr: u32, body: &[u8], flags: u32, memsz: u32) -> Vec<u8> {
        let phoff = EHDR_SIZE;
        let body_at = phoff + PHDR_SIZE;
        let mut out = vec![0u8; body_at];
        out[..4].copy_from_slice(&ELF_MAGIC);
        out[4] = ELFCLASS32;
        out[5] = ELFDATA2LSB;
        out[16..18].copy_from_slice(&2u16.to_le_bytes());
        out[18..20].copy_from_slice(&EM_RISCV.to_le_bytes());
        out[24..28].copy_from_slice(&entry.to_le_bytes());
        out[28..32].copy_from_slice(&(phoff as u32).to_le_bytes());
        out[42..44].copy_from_slice(&(PHDR_SIZE as u16).to_le_bytes());
        out[44..46].copy_from_slice(&1u16.to_le_bytes());

        let ph = &mut out[phoff..phoff + PHDR_SIZE];
        ph[0..4].copy_from_slice(&PT_LOAD.to_le_bytes());
        ph[4..8].copy_from_slice(&(body_at as u32).to_le_bytes());
        ph[8..12].copy_from_slice(&vaddr.to_le_bytes());
        ph[16..20].copy_from_slice(&(body.len() as u32).to_le_bytes());
        ph[20..24].copy_from_slice(&memsz.to_le_bytes());
        ph[24..28].copy_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn a_flat_image_is_its_bytes_at_the_address_it_is_given() {
        let image = Image::flat(vec![1, 2, 3, 4], RAM_BASE);
        assert_eq!(image.entry, RAM_BASE);
        assert_eq!(image.segments.len(), 1);
        assert_eq!(image.segments[0].vaddr, RAM_BASE);
        assert_eq!(image.text_region(), Some((RAM_BASE, RAM_BASE + 4)));
    }

    #[test]
    fn an_elf_says_where_its_own_pieces_go() {
        let bytes = elf_with(0x8000_0010, 0x8000_0000, &[1, 2, 3, 4], PF_X | 4, 8);
        let image = Image::parse_elf(&bytes).unwrap();
        assert_eq!(image.entry, 0x8000_0010);
        assert_eq!(image.segments.len(), 1);
        assert_eq!(image.segments[0].bytes, [1, 2, 3, 4]);
        assert_eq!(image.segments[0].mem_len, 8, "the zero-filled tail is kept");
        assert_eq!(image.text_region(), Some((0x8000_0000, 0x8000_0008)));
    }

    #[test]
    fn a_segment_that_cannot_be_executed_is_not_part_of_the_text_region() {
        let bytes = elf_with(0, 0x8000_0000, &[1, 2, 3, 4], 4 | 2, 4);
        let image = Image::parse_elf(&bytes).unwrap();
        assert_eq!(image.text_region(), None);
    }

    #[test]
    fn a_file_that_is_not_an_elf_is_read_flat() {
        let image = read_image(vec![0xDE, 0xAD], Some(RAM_BASE)).unwrap();
        assert_eq!(image.segments[0].bytes, [0xDE, 0xAD]);
        assert!(!Image::looks_like_elf(&[0xDE, 0xAD]));
    }

    #[test]
    fn a_malformed_elf_is_an_error_rather_than_a_panic() {
        assert_eq!(
            Image::parse_elf(&[0x7F, b'E']),
            Err(ImageError::Truncated {
                at: 2,
                what: "the file header"
            })
        );
        let mut wrong_class = elf_with(0, 0, &[0], PF_X, 1);
        wrong_class[4] = 2;
        assert_eq!(Image::parse_elf(&wrong_class), Err(ImageError::WrongClass));

        let mut wrong_machine = elf_with(0, 0, &[0], PF_X, 1);
        wrong_machine[18..20].copy_from_slice(&62u16.to_le_bytes());
        assert_eq!(
            Image::parse_elf(&wrong_machine),
            Err(ImageError::WrongMachine(62))
        );

        // A segment claiming more file than memory, and one running past the
        // end of the file.
        assert_eq!(
            Image::parse_elf(&elf_with(0, 0, &[1, 2, 3, 4], PF_X, 2)),
            Err(ImageError::SegmentTooLong {
                vaddr: 0,
                filesz: 4,
                memsz: 2
            })
        );
        let mut truncated = elf_with(0, 0, &[1, 2, 3, 4], PF_X, 4);
        truncated.truncate(truncated.len() - 2);
        assert!(matches!(
            Image::parse_elf(&truncated),
            Err(ImageError::Truncated { .. })
        ));
    }

    #[test]
    fn an_elf_with_nothing_to_load_is_an_error() {
        let mut none = elf_with(0, 0, &[1], PF_X, 1);
        none[44..46].copy_from_slice(&0u16.to_le_bytes());
        assert_eq!(Image::parse_elf(&none), Err(ImageError::NoSegments));
    }

    #[test]
    fn no_byte_of_a_truncated_file_makes_the_reader_panic() {
        // Every prefix of a well-formed file, so the bounds checks are
        // exercised at every offset a header field can sit at.
        let full = elf_with(0x8000_0000, 0x8000_0000, &[1, 2, 3, 4], PF_X, 4);
        for len in 0..full.len() {
            let _ = Image::parse_elf(&full[..len]);
        }
        // And every single-byte corruption of it.
        for at in 0..full.len() {
            let mut bad = full.clone();
            bad[at] = bad[at].wrapping_add(0x7F);
            let _ = Image::parse_elf(&bad);
        }
    }
}
