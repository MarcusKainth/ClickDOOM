//! Checking a WAD against a committed `sha256sum` line.

use clickdoom_spec::sha256_hex;

use super::error::WadError;

/// The `sha256sum` output for the shareware WAD this crate targets, as
/// `rom/wad/doom1.wad.sha256sum` holds it.
pub const DOOM1_SHA256SUM: &str = include_str!("../../../rom/wad/doom1.wad.sha256sum");

/// The 64 hex digits at the head of a `sha256sum` line.
fn expected_digest(line: &str) -> Result<&str, WadError> {
    let bad = || WadError::BadChecksumFile {
        line: line.trim_end().to_owned(),
    };
    let digest = line.split_whitespace().next().ok_or_else(bad)?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    Ok(digest)
}

/// Checks `bytes` against the digest in a `sha256sum` line.
///
/// The whole file is hashed, so a caller holding the WAD in memory pays one
/// pass over it. Nothing else in this crate looks at the digest: parsing a
/// WAD works on any WAD, and this says whether it is the expected one.
pub fn verify(bytes: &[u8], sha256sum_line: &str) -> Result<(), WadError> {
    let expected = expected_digest(sha256sum_line)?;
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(WadError::ChecksumMismatch {
            expected: expected.to_owned(),
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_digest_off_a_sha256sum_line() {
        let line = "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771  doom1.wad\n";
        assert_eq!(expected_digest(line).unwrap(), &line[..64]);
    }

    #[test]
    fn rejects_a_line_that_is_not_a_digest() {
        assert!(expected_digest("").is_err());
        assert!(expected_digest("not-a-digest  doom1.wad\n").is_err());
        assert!(expected_digest("abcd  doom1.wad\n").is_err());
    }

    #[test]
    fn empty_input_does_not_match_the_committed_digest() {
        let err = verify(b"", DOOM1_SHA256SUM).unwrap_err();
        assert!(matches!(err, WadError::ChecksumMismatch { .. }));
    }
}
