//! Reading a WAD's header and directory into lump rows.
//!
//! Parsing stops at the directory. A [`Lump`] carries its bytes as an
//! unread slice, and everything a lump's bytes mean is decoded in SQL: map
//! records, texture composition, sprite frames, the demo's tic commands.
//! Nothing here looks inside a lump.
//!
//! The rows borrow the WAD buffer, so a full read of `doom1.wad` allocates
//! only the directory.

pub mod checksum;
pub mod error;
pub mod marker;
pub mod name;

pub use checksum::DOOM1_SHA256SUM;
pub use error::WadError;

/// Bytes of the header: magic, lump count, directory offset.
const HEADER_LEN: usize = 12;

/// Bytes of one directory entry: offset, size, name.
const ENTRY_LEN: usize = 16;

/// Which kind of WAD the magic says this is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WadKind {
    /// A complete game's data.
    Iwad,
    /// A patch WAD, replacing lumps of an IWAD.
    Pwad,
}

/// One lump: its directory entry, and its bytes as they lie in the file.
#[derive(Clone, Copy, Debug)]
pub struct Lump<'a> {
    /// Position in the directory, counted from 0. A name can repeat, so
    /// this is the only unique key.
    pub index: u32,
    /// The name, trimmed at its NUL terminator.
    pub name: &'a str,
    /// The name of the map marker enclosing this lump, empty for a lump
    /// no map owns. A marker carries its own name.
    pub map_marker: &'a str,
    /// The lump's bytes, undecoded.
    pub bytes: &'a [u8],
}

/// A WAD's header and directory.
#[derive(Clone, Debug)]
pub struct Wad<'a> {
    kind: WadKind,
    lumps: Vec<Lump<'a>>,
}

impl<'a> Wad<'a> {
    /// Reads `bytes` as a WAD.
    ///
    /// Every directory entry is checked to lie inside `bytes`, so a
    /// [`Lump`]'s slice is always readable. A map lump found outside any
    /// marker is an error: the SQL side selects a map's lumps by marker,
    /// and a lump nothing can select is a directory this reader has
    /// misread.
    pub fn parse(bytes: &'a [u8]) -> Result<Wad<'a>, WadError> {
        let header = bytes
            .first_chunk::<HEADER_LEN>()
            .ok_or(WadError::HeaderTooShort { len: bytes.len() })?;
        let [magic @ .., c0, c1, c2, c3, o0, o1, o2, o3] = header;
        let kind = match magic {
            b"IWAD" => WadKind::Iwad,
            b"PWAD" => WadKind::Pwad,
            magic => return Err(WadError::BadMagic { magic: *magic }),
        };
        let count = read_i32(&[*c0, *c1, *c2, *c3]);
        let offset = read_i32(&[*o0, *o1, *o2, *o3]);
        if count < 0 {
            return Err(WadError::NegativeLumpCount { count });
        }
        if offset < 0 {
            return Err(WadError::NegativeDirectoryOffset { offset });
        }
        let (count, offset) = (count as u64, offset as u64);

        // `directory_slice` returns exactly `count` entries' worth of
        // bytes, so the remainder `as_chunks` splits off is empty.
        let (entries, _) = directory_slice(bytes, offset, count)?.as_chunks::<ENTRY_LEN>();
        let mut lumps = Vec::with_capacity(count as usize);
        let mut current_marker = "";
        for (index, entry) in entries.iter().enumerate() {
            let index = index as u32;
            let lump = read_entry(bytes, index, entry)?;
            if marker::is_marker(lump.name) {
                current_marker = lump.name;
            } else if !marker::is_map_lump(lump.name) {
                current_marker = "";
            } else if current_marker.is_empty() {
                return Err(WadError::OrphanMapLump {
                    index,
                    name: lump.name.to_owned(),
                });
            }
            lumps.push(Lump {
                map_marker: current_marker,
                ..lump
            });
        }
        Ok(Wad { kind, lumps })
    }

    /// Reads `bytes` as a WAD after checking it against `sha256sum_line`.
    pub fn parse_verified(bytes: &'a [u8], sha256sum_line: &str) -> Result<Wad<'a>, WadError> {
        checksum::verify(bytes, sha256sum_line)?;
        Wad::parse(bytes)
    }

    pub fn kind(&self) -> WadKind {
        self.kind
    }

    /// Every lump, in directory order.
    pub fn lumps(&self) -> &[Lump<'a>] {
        &self.lumps
    }

    /// The last lump named `name`.
    ///
    /// Last, not first: the engine's own `W_CheckNumForName` resolves a
    /// name to the highest-numbered lump carrying it, so a later lump
    /// replaces an earlier one of the same name. `doom1.wad` holds two
    /// lumps named `SW18_7`, and a texture that names that patch gets the
    /// second.
    ///
    /// A map lump name repeats once per map, and
    /// [`map_lump`](Wad::map_lump) is what reads one of those.
    pub fn find(&self, name: &str) -> Option<&Lump<'a>> {
        self.lumps.iter().rev().find(|l| l.name == name)
    }

    /// The lumps enclosed by the marker `map_marker`, in directory order,
    /// the marker itself first.
    pub fn map(&self, map_marker: &str) -> impl Iterator<Item = &Lump<'a>> {
        self.lumps
            .iter()
            .filter(move |l| l.map_marker == map_marker)
    }

    /// The lump named `name` inside the map `map_marker`.
    pub fn map_lump(&self, map_marker: &str, name: &str) -> Option<&Lump<'a>> {
        self.map(map_marker).find(|l| l.name == name)
    }
}

fn read_i32(bytes: &[u8; 4]) -> i32 {
    i32::from_le_bytes(*bytes)
}

/// A directory entry's three fields: body offset, body size, name bytes.
/// The name borrows the entry.
fn split_entry(entry: &[u8; ENTRY_LEN]) -> (i32, i32, &[u8; name::NAME_LEN]) {
    let [o0, o1, o2, o3, s0, s1, s2, s3, name @ ..] = entry;
    (
        i32::from_le_bytes([*o0, *o1, *o2, *o3]),
        i32::from_le_bytes([*s0, *s1, *s2, *s3]),
        name,
    )
}

/// The directory's own bytes, checked to lie inside the WAD.
fn directory_slice(bytes: &[u8], offset: u64, count: u64) -> Result<&[u8], WadError> {
    let out_of_range = || WadError::DirectoryOutOfRange {
        offset,
        count,
        len: bytes.len(),
    };
    let span = count
        .checked_mul(ENTRY_LEN as u64)
        .and_then(|span| span.checked_add(offset))
        .ok_or_else(out_of_range)?;
    let (start, end) = (
        usize::try_from(offset).map_err(|_| out_of_range())?,
        usize::try_from(span).map_err(|_| out_of_range())?,
    );
    bytes.get(start..end).ok_or_else(out_of_range)
}

/// One directory entry, with its bytes checked to lie inside the WAD. The
/// `map_marker` is filled in by the caller, which is the only place the
/// enclosing marker is known.
fn read_entry<'a>(
    bytes: &'a [u8],
    index: u32,
    entry: &'a [u8; ENTRY_LEN],
) -> Result<Lump<'a>, WadError> {
    let (offset, size, field) = split_entry(entry);
    let name = name::read(index, field)?;
    if offset < 0 || size < 0 {
        return Err(WadError::NegativeLumpExtent {
            index,
            offset,
            size,
        });
    }
    let (offset, size) = (offset as usize, size as usize);
    let lump_bytes = offset
        .checked_add(size)
        .and_then(|end| bytes.get(offset..end))
        .ok_or_else(|| WadError::LumpOutOfRange {
            index,
            name: name.to_owned(),
            offset: offset as u64,
            size: size as u64,
            len: bytes.len(),
        })?;
    Ok(Lump {
        index,
        name,
        map_marker: "",
        bytes: lump_bytes,
    })
}

#[cfg(test)]
mod tests;
