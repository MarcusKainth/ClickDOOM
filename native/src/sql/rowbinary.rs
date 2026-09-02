//! Encoding rows in ClickHouse's RowBinary format.
//!
//! Only the types the WAD insert and the render pipeline's input row use.
//! RowBinary carries no column names and no types, so the statement's own
//! column list is what says which value is which.

/// A `UInt8`.
pub fn u8(out: &mut Vec<u8>, value: u8) {
    out.push(value);
}

/// A `UInt32`, little-endian.
pub fn u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// A `String`: its length as a LEB128 varint, then its bytes. The bytes
/// are not text, and ClickHouse does not treat them as text either.
pub fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_little_endian_word() {
        let mut out = Vec::new();
        u32(&mut out, 0x0403_0201);
        assert_eq!(out, [1, 2, 3, 4]);
    }

    #[test]
    fn a_length_under_128_is_one_byte() {
        let mut out = Vec::new();
        string(&mut out, b"E1M7");
        assert_eq!(out, [4, b'E', b'1', b'M', b'7']);
    }

    /// A lump runs to tens of kilobytes, so the multi-byte varint is the
    /// case that carries the real data.
    #[test]
    fn a_longer_length_continues_seven_bits_at_a_time() {
        let mut out = Vec::new();
        varint(&mut out, 127);
        varint(&mut out, 128);
        varint(&mut out, 300);
        varint(&mut out, 64_000);
        assert_eq!(out, [0x7f, 0x80, 0x01, 0xac, 0x02, 0x80, 0xf4, 0x03]);
    }

    #[test]
    fn a_byte_is_itself() {
        let mut out = Vec::new();
        u8(&mut out, 40);
        assert_eq!(out, [40]);
    }

    #[test]
    fn a_string_may_hold_any_byte() {
        let mut out = Vec::new();
        string(&mut out, &[0, 0xff, b'\n']);
        assert_eq!(out, [3, 0, 0xff, b'\n']);
    }
}
