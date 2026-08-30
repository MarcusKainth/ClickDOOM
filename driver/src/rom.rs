//! Loading a ROM's raw bytes into `ram`.
//!
//! Loading reinterprets the flat binary as little-endian 32-bit words and
//! inserts them at their word address. It never inspects an opcode field,
//! never branches on instruction content: decoding is `sqlcpu/decode.sql`'s
//! job, a separate SQL step over the rows this writes.
//!
//! Once loaded, `ram` holds exactly one row for every word in the RAM
//! region: the image, then explicit zeros for the rest. Both the fold's
//! `RAMT` and the SQL CPU's own reads index the captured RAM array
//! positionally, so a sparse `ram` silently reads the wrong word past the
//! first gap, with no error and no halt. Zero-filling the tail keeps `ram`
//! dense by construction rather than by the accident of a boot path that
//! happens to touch every word.

use std::path::{Path, PathBuf};

use clickdoom_spec::{Manifest, RAM_BASE, RAM_SIZE, manifest::ManifestError, sha256_hex};
use clickhouse::Row;
use serde::Serialize;

use crate::client::{Db, Error};

/// One row of `ram`/`framebuffer`/`palette`: all three share this exact
/// shape (word_addr, value, version), ReplacingMergeTree keyed on
/// word_addr.
#[derive(Row, Serialize)]
pub(crate) struct WordRow {
    pub word_addr: u32,
    pub value: u32,
    pub version: u64,
}

/// Word count of the RAM region a freshly loaded `ram` is dense over.
pub const RAM_WORDS_DEFAULT: u32 = RAM_SIZE / 4;

#[derive(Debug, thiserror::Error)]
pub enum LoadRomError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("{path}: size {actual} bytes != manifest size {expected} bytes")]
    SizeMismatch {
        path: PathBuf,
        actual: u64,
        expected: u64,
    },
    #[error("{path}: sha256 {actual} != manifest sha256 {expected}")]
    HashMismatch {
        path: PathBuf,
        actual: String,
        expected: String,
    },
    #[error("{path}: length {len} bytes is not a multiple of 4")]
    Unaligned { path: PathBuf, len: u64 },
    #[error("manifest load_addr {0:#010x} is not word-aligned")]
    LoadAddrUnaligned(u32),
    #[error(
        "image ends at word_addr {image_end}, past the {ram_words}-word RAM region ending at {region_end}"
    )]
    Overflow {
        image_end: u32,
        ram_words: u32,
        region_end: u32,
    },
    #[error(transparent)]
    Db(#[from] Error),
    #[error(
        "ram is not dense over the RAM region: {rows} rows spanning {span} words from {lowest}, expected {ram_words} rows spanning {ram_words} words from {base_word}"
    )]
    NotDense {
        rows: u64,
        span: u64,
        lowest: u64,
        ram_words: u32,
        base_word: u32,
    },
}

/// What loading a ROM produced, for the caller to report.
pub struct Loaded {
    pub words: u32,
    pub bytes: u64,
    pub base_word: u32,
    pub ram_words: u32,
}

/// Loads `bin` into `ram`, checked against `manifest_path`, then zero-fills
/// the rest of `ram_words` so the table is dense from the image's base word.
pub async fn load(
    db: &Db,
    bin: &Path,
    manifest_path: &Path,
    ram_words: u32,
) -> Result<Loaded, LoadRomError> {
    let manifest = Manifest::read(manifest_path)?;

    let blob = std::fs::read(bin).map_err(|source| LoadRomError::Read {
        path: bin.to_owned(),
        source,
    })?;

    if let Some(expected) = manifest.size
        && blob.len() as u64 != expected
    {
        return Err(LoadRomError::SizeMismatch {
            path: bin.to_owned(),
            actual: blob.len() as u64,
            expected,
        });
    }

    let digest = sha256_hex(&blob);
    if let Some(expected) = &manifest.sha256
        && &digest != expected
    {
        return Err(LoadRomError::HashMismatch {
            path: bin.to_owned(),
            actual: digest,
            expected: expected.clone(),
        });
    }

    if blob.len() % 4 != 0 {
        return Err(LoadRomError::Unaligned {
            path: bin.to_owned(),
            len: blob.len() as u64,
        });
    }

    let load_addr = manifest.load_addr.unwrap_or(RAM_BASE);
    if load_addr % 4 != 0 {
        return Err(LoadRomError::LoadAddrUnaligned(load_addr));
    }

    let base_word = load_addr / 4;
    let word_count = (blob.len() / 4) as u32;
    let rows = blob.chunks_exact(4).enumerate().map(|(i, w)| WordRow {
        word_addr: base_word + i as u32,
        value: u32::from_le_bytes([w[0], w[1], w[2], w[3]]),
        version: 0,
    });
    db.insert_all("ram", rows).await?;

    let fill_start = base_word + word_count;
    let region_end = base_word + ram_words;
    if fill_start > region_end {
        return Err(LoadRomError::Overflow {
            image_end: fill_start,
            ram_words,
            region_end,
        });
    }
    if fill_start < region_end {
        let fill_count = region_end - fill_start;
        db.run(&format!(
            "INSERT INTO ram (word_addr, value, version) \
             SELECT toUInt32({fill_start} + number), toUInt32(0), toUInt64(0) \
             FROM numbers({fill_count})"
        ))
        .await?;
    }

    let (rows, span, lowest): (u64, u64, u64) = db
        .fetch_one(
            "SELECT count(), toUInt64(max(word_addr) - min(word_addr) + 1), toUInt64(min(word_addr)) FROM ram FINAL",
        )
        .await?;
    if rows != span || rows != ram_words as u64 || lowest != base_word as u64 {
        return Err(LoadRomError::NotDense {
            rows,
            span,
            lowest,
            ram_words,
            base_word,
        });
    }

    Ok(Loaded {
        words: word_count,
        bytes: blob.len() as u64,
        base_word,
        ram_words,
    })
}
