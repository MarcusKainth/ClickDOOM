//! The 8-byte lump name field.

use super::error::WadError;

/// Length of the name field in a directory entry.
pub const NAME_LEN: usize = 8;

/// Reads the name out of a directory entry's 8 name bytes.
///
/// The name is the run of printable ASCII before the first NUL. Every byte
/// after that NUL has to be NUL as well, so a name never depends on
/// whatever the writer left in the padding. The result borrows the
/// directory, so reading a whole WAD's names allocates nothing.
pub fn read(index: u32, field: &[u8; NAME_LEN]) -> Result<&str, WadError> {
    let end = field.iter().position(|b| *b == 0).unwrap_or(NAME_LEN);
    let bad_padding = field[end..].iter().any(|b| *b != 0);
    let bad_chars = field[..end].iter().any(|b| !b.is_ascii_graphic());
    let bad_name = || WadError::BadName {
        index,
        bytes: *field,
    };
    if end == 0 || bad_padding || bad_chars {
        return Err(bad_name());
    }
    std::str::from_utf8(&field[..end]).map_err(|_| bad_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_at_the_first_nul() {
        assert_eq!(read(0, b"E1M7\0\0\0\0").unwrap(), "E1M7");
        assert_eq!(read(0, b"BLOCKMAP").unwrap(), "BLOCKMAP");
    }

    #[test]
    fn rejects_a_byte_after_the_terminator() {
        assert!(matches!(
            read(3, b"SEGS\0X\0\0"),
            Err(WadError::BadName { index: 3, .. })
        ));
    }

    #[test]
    fn rejects_an_empty_or_unprintable_name() {
        assert!(read(0, b"\0\0\0\0\0\0\0\0").is_err());
        assert!(read(0, b"BAD\x7fLUMP").is_err());
        assert!(read(0, b"HAS SPAC").is_err());
    }
}
