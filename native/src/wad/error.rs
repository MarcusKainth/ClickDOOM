//! What a malformed WAD looks like as a typed error.

/// A WAD that cannot be turned into lump rows, and where the problem is.
///
/// Every variant names the byte range or the lump index a reader can look
/// at, so a report of one is reproducible without the file.
#[derive(Debug, thiserror::Error)]
pub enum WadError {
    #[error("{len} bytes is shorter than the 12-byte header")]
    HeaderTooShort { len: usize },

    #[error("magic {magic:?} is neither IWAD nor PWAD")]
    BadMagic { magic: [u8; 4] },

    #[error("header declares {count} lumps")]
    NegativeLumpCount { count: i32 },

    #[error("header puts the directory at offset {offset}")]
    NegativeDirectoryOffset { offset: i32 },

    #[error("directory of {count} lumps at offset {offset} runs past {len} bytes")]
    DirectoryOutOfRange { offset: u64, count: u64, len: usize },

    #[error("lump {index} is {size} bytes at offset {offset}")]
    NegativeLumpExtent { index: u32, offset: i32, size: i32 },

    #[error("lump {index} ({name}) of {size} bytes at offset {offset} runs past {len} bytes")]
    LumpOutOfRange {
        index: u32,
        name: String,
        offset: u64,
        size: u64,
        len: usize,
    },

    #[error("lump {index} has the name bytes {bytes:?}")]
    BadName { index: u32, bytes: [u8; 8] },

    #[error("lump {index} ({name}) is a map lump outside any map marker")]
    OrphanMapLump { index: u32, name: String },

    #[error("checksum file holds {line:?}, which is not a sha256sum line")]
    BadChecksumFile { line: String },

    #[error("sha256 {actual} does not match the expected {expected}")]
    ChecksumMismatch { expected: String, actual: String },
}
