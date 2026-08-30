//! The ROM manifest, its pinned hash, and the filename convention that keeps a
//! stale artifact from looking current.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How many hex characters of a ROM's sha256 name an artifact generated from
/// it. The same width git uses for a short hash.
pub const HASH_PREFIX_LEN: usize = 12;

/// What the ROM build emits beside the image. Every field is optional: a
/// binary that is not this ROM may declare only some of them, and the emulator
/// falls back to its own defaults for the rest.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_addr: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The read-only text region's bounds, half open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_end: Option<u32>,
}

impl Manifest {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn read(path: &Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_json(&text).map_err(|source| ManifestError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// The text bounds, when the manifest declares both. One without the other
    /// leaves the region undeclared rather than half declared.
    pub fn text_region(&self) -> Option<(u32, u32)> {
        match (self.text_start, self.text_end) {
            (Some(start), Some(end)) => Some((start, end)),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("reading {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Lowercase hex sha256, the spelling `PINNED_HASH` and every manifest use.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut stream = Sha256Stream::new();
    stream.update(bytes);
    stream.finish()
}

/// A sha256 fed in pieces, for a caller that hashes an output while it writes
/// it rather than reading the file back afterwards. `finish` returns the same
/// spelling as [`sha256_hex`].
#[derive(Default)]
pub struct Sha256Stream(Sha256);

impl Sha256Stream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, bytes: &[u8]) {
        Digest::update(&mut self.0, bytes);
    }

    pub fn finish(self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.0.finalize() {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PinnedHashError {
    #[error("reading {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is empty")]
    Empty { path: PathBuf },
    #[error("image sha256 is {actual}, but {path} pins {pinned}")]
    Mismatch {
        path: PathBuf,
        actual: String,
        pinned: String,
    },
}

/// Fails unless `image` hashes to what `pinned_hash_path` names. Returns the
/// hash on success, so a caller can name its output after the image it
/// actually read rather than the one it meant to.
pub fn assert_pinned_hash(
    image: &[u8],
    pinned_hash_path: &Path,
) -> Result<String, PinnedHashError> {
    let pinned =
        std::fs::read_to_string(pinned_hash_path).map_err(|source| PinnedHashError::Read {
            path: pinned_hash_path.to_owned(),
            source,
        })?;
    let pinned = pinned.trim().to_owned();
    if pinned.is_empty() {
        return Err(PinnedHashError::Empty {
            path: pinned_hash_path.to_owned(),
        });
    }
    let actual = sha256_hex(image);
    if actual != pinned {
        return Err(PinnedHashError::Mismatch {
            path: pinned_hash_path.to_owned(),
            actual,
            pinned,
        });
    }
    Ok(actual)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("a ROM hash needs at least {HASH_PREFIX_LEN} characters to name an artifact, got {0:?}")]
pub struct ShortHash(pub String);

/// `<prefix>.<first twelve of the ROM's sha256><suffix>`.
///
/// The hash goes in the name and not only in a sidecar, so an artifact
/// generated against one ROM cannot be mistaken for current after the pinned
/// ROM moves.
pub fn hashed_filename(prefix: &str, rom_sha256: &str, suffix: &str) -> Result<String, ShortHash> {
    if rom_sha256.len() < HASH_PREFIX_LEN {
        return Err(ShortHash(rom_sha256.to_owned()));
    }
    Ok(format!(
        "{prefix}.{}{suffix}",
        &rom_sha256[..HASH_PREFIX_LEN]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROM_SHA: &str = "9a6a47d01119f67580e48e9875207186c25efd56ff93019df331eb307cfaa5d9";

    #[test]
    fn sha256_of_nothing_is_the_published_digest() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_stream_hashes_the_same_as_one_slice() {
        let mut stream = Sha256Stream::new();
        stream.update(b"a");
        stream.update(b"bc");
        assert_eq!(stream.finish(), sha256_hex(b"abc"));
    }

    #[test]
    fn an_artifact_is_named_after_the_rom_it_came_from() {
        assert_eq!(
            hashed_filename("demo-boot-to-first-frame", ROM_SHA, ".tsv").unwrap(),
            "demo-boot-to-first-frame.9a6a47d01119.tsv"
        );
        assert_eq!(
            hashed_filename("demo3", ROM_SHA, ".json").unwrap(),
            "demo3.9a6a47d01119.json"
        );
    }

    #[test]
    fn a_hash_too_short_to_name_an_artifact_is_an_error() {
        assert_eq!(
            hashed_filename("demo3", "9a6a47d0", ".tsv"),
            Err(ShortHash("9a6a47d0".to_owned()))
        );
    }

    #[test]
    fn the_real_manifest_parses() {
        let manifest = Manifest::from_json(
            r#"{"spec_version": "0.1.0", "entry": 2147483648, "load_addr": 2147483648,
                "size": 4789984, "sha256": "9a6a", "text_start": 2147483648,
                "text_end": 2147879460}"#,
        )
        .unwrap();
        assert_eq!(manifest.load_addr, Some(0x8000_0000));
        assert_eq!(manifest.text_region(), Some((2147483648, 2147879460)));
        assert_eq!(manifest.spec_version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn a_manifest_declaring_nothing_parses_and_declares_no_text_region() {
        let manifest = Manifest::from_json("{}").unwrap();
        assert_eq!(manifest, Manifest::default());
        assert_eq!(manifest.text_region(), None);
    }

    #[test]
    fn one_text_bound_alone_leaves_the_region_undeclared() {
        let manifest = Manifest::from_json(r#"{"text_start": 2147483648}"#).unwrap();
        assert_eq!(manifest.text_region(), None);
    }

    #[test]
    fn a_field_the_manifest_does_not_know_is_ignored_rather_than_fatal() {
        let manifest = Manifest::from_json(r#"{"load_addr": 1, "future_field": 2}"#).unwrap();
        assert_eq!(manifest.load_addr, Some(1));
    }
}
