//! RowBinary encoding for the rows a resident statement's `input()` reads.
//!
//! Integers are little-endian and fixed width. A `String` is an unsigned
//! LEB128 byte count followed by the bytes. Columns appear in the order the
//! input schema declares them, with nothing framing the row.

use bytes::{BufMut, BytesMut};

/// One row under construction. Append a value per column of the input
/// schema, in schema order, then [`finish`](Row::finish).
#[derive(Debug, Default)]
pub struct Row {
    buf: BytesMut,
}

impl Row {
    /// An empty row.
    pub fn new() -> Row {
        Row::default()
    }

    /// An empty row with room for `bytes` before it reallocates.
    pub fn with_capacity(bytes: usize) -> Row {
        Row {
            buf: BytesMut::with_capacity(bytes),
        }
    }

    /// Appends a `UInt8`.
    pub fn u8(&mut self, value: u8) -> &mut Row {
        self.buf.put_u8(value);
        self
    }

    /// Appends an `Int8`.
    pub fn i8(&mut self, value: i8) -> &mut Row {
        self.buf.put_i8(value);
        self
    }

    /// Appends a `UInt16`.
    pub fn u16(&mut self, value: u16) -> &mut Row {
        self.buf.put_u16_le(value);
        self
    }

    /// Appends an `Int16`.
    pub fn i16(&mut self, value: i16) -> &mut Row {
        self.buf.put_i16_le(value);
        self
    }

    /// Appends a `UInt32`.
    pub fn u32(&mut self, value: u32) -> &mut Row {
        self.buf.put_u32_le(value);
        self
    }

    /// Appends an `Int32`.
    pub fn i32(&mut self, value: i32) -> &mut Row {
        self.buf.put_i32_le(value);
        self
    }

    /// Appends a `UInt64`.
    pub fn u64(&mut self, value: u64) -> &mut Row {
        self.buf.put_u64_le(value);
        self
    }

    /// Appends an `Int64`.
    pub fn i64(&mut self, value: i64) -> &mut Row {
        self.buf.put_i64_le(value);
        self
    }

    /// Appends a `String`: its length as LEB128, then its bytes. Takes
    /// bytes rather than `&str`, because a ClickHouse `String` holds
    /// arbitrary bytes.
    pub fn bytes(&mut self, value: &[u8]) -> &mut Row {
        put_leb128(&mut self.buf, value.len() as u64);
        self.buf.put_slice(value);
        self
    }

    /// How many bytes the row holds so far.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether no column has been appended.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The encoded row.
    pub fn finish(self) -> bytes::Bytes {
        self.buf.freeze()
    }
}

/// Appends `value` as unsigned LEB128: seven bits per byte, low group
/// first, the high bit set on every byte but the last.
pub fn put_leb128(buf: &mut BytesMut, mut value: u64) {
    loop {
        let group = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            buf.put_u8(group);
            return;
        }
        buf.put_u8(group | 0x80);
    }
}

/// A column of an input schema: the type is what decides the padding row's
/// encoding.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum ColumnType {
    /// Width in bytes. Signedness does not change the zero encoding.
    Fixed(usize),
    Text,
}

/// An input schema this module cannot encode a padding row for.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("column {column:?} of the input schema is not `<name> <Type>`")]
    Shape { column: String },
    #[error(
        "column {column:?} has type {found:?}; the padding row can encode \
         UInt8/16/32/64, Int8/16/32/64 and String"
    )]
    Type { column: String, found: String },
    #[error(
        "the input schema has no String column, so the padding row cannot \
         reach {PADDING_BYTES} bytes; give the schema a String column the \
         statement ignores"
    )]
    NoText,
}

/// How many bytes of padding the String column of the padding row carries.
/// The server reads `max_query_size` bytes before it parses, which is the
/// statement plus 64, so the padding row has to cover that remainder on its
/// own.
pub const PADDING_BYTES: usize = 128;

/// The first row of a resident statement's body: every column at its zero
/// value, and every `String` column filled to [`PADDING_BYTES`].
///
/// A statement drops this row with `WHERE tic > 0`.
pub fn padding_row(input_schema: &str) -> Result<bytes::Bytes, SchemaError> {
    let columns = parse_schema(input_schema)?;
    if !columns.contains(&ColumnType::Text) {
        return Err(SchemaError::NoText);
    }
    let pad = vec![b'0'; PADDING_BYTES];
    let mut row = Row::with_capacity(input_schema.len() + PADDING_BYTES);
    for column in columns {
        match column {
            ColumnType::Fixed(width) => {
                for _ in 0..width {
                    row.u8(0);
                }
            }
            ColumnType::Text => {
                row.bytes(&pad);
            }
        }
    }
    Ok(row.finish())
}

/// Splits `input_schema` on commas and reads the type of each `<name>
/// <Type>` pair.
fn parse_schema(input_schema: &str) -> Result<Vec<ColumnType>, SchemaError> {
    input_schema
        .split(',')
        .map(str::trim)
        .filter(|column| !column.is_empty())
        .map(|column| {
            let (_, type_name) =
                column
                    .rsplit_once(char::is_whitespace)
                    .ok_or_else(|| SchemaError::Shape {
                        column: column.to_owned(),
                    })?;
            column_type(type_name).ok_or_else(|| SchemaError::Type {
                column: column.to_owned(),
                found: type_name.to_owned(),
            })
        })
        .collect()
}

fn column_type(type_name: &str) -> Option<ColumnType> {
    match type_name {
        "UInt8" | "Int8" => Some(ColumnType::Fixed(1)),
        "UInt16" | "Int16" => Some(ColumnType::Fixed(2)),
        "UInt32" | "Int32" => Some(ColumnType::Fixed(4)),
        "UInt64" | "Int64" => Some(ColumnType::Fixed(8)),
        "String" => Some(ColumnType::Text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leb128(value: u64) -> Vec<u8> {
        let mut buf = BytesMut::new();
        put_leb128(&mut buf, value);
        buf.to_vec()
    }

    #[test]
    fn integers_are_little_endian_and_fixed_width() {
        let mut row = Row::new();
        row.u8(0xab)
            .i8(-2)
            .u16(0x1234)
            .i16(-2)
            .u32(0x0123_4567)
            .i32(-2)
            .u64(0x0123_4567_89ab_cdef)
            .i64(-2);
        assert_eq!(
            row.finish().to_vec(),
            vec![
                0xab, // UInt8
                0xfe, // Int8
                0x34, 0x12, // UInt16
                0xfe, 0xff, // Int16
                0x67, 0x45, 0x23, 0x01, // UInt32
                0xfe, 0xff, 0xff, 0xff, // Int32
                0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01, // UInt64
                0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, // Int64
            ]
        );
    }

    #[test]
    fn leb128_uses_seven_bits_a_byte_low_group_first() {
        assert_eq!(leb128(0), vec![0x00]);
        assert_eq!(leb128(1), vec![0x01]);
        assert_eq!(leb128(127), vec![0x7f]);
        assert_eq!(leb128(128), vec![0x80, 0x01]);
        assert_eq!(leb128(300), vec![0xac, 0x02]);
        assert_eq!(leb128(16_383), vec![0xff, 0x7f]);
        assert_eq!(leb128(16_384), vec![0x80, 0x80, 0x01]);
        assert_eq!(
            leb128(u64::MAX),
            vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]
        );
    }

    #[test]
    fn a_string_is_its_length_then_its_bytes() {
        let mut row = Row::new();
        row.bytes(b"doom");
        assert_eq!(row.finish().to_vec(), b"\x04doom".to_vec());

        let long = vec![b'x'; 200];
        let mut row = Row::new();
        row.bytes(&long);
        let encoded = row.finish();
        assert_eq!(&encoded[..2], &[0xc8, 0x01]);
        assert_eq!(&encoded[2..], &long[..]);
    }

    #[test]
    fn a_string_holds_bytes_that_are_not_utf8() {
        let mut row = Row::new();
        row.bytes(&[0xff, 0x00, 0xfe]);
        assert_eq!(row.finish().to_vec(), vec![0x03, 0xff, 0x00, 0xfe]);
    }

    #[test]
    fn the_padding_row_zeroes_every_column_and_fills_the_string() {
        let row = padding_row("tic UInt32, pad String").expect("a schema with a String column");
        let mut expected = vec![0u8; 4];
        expected.extend_from_slice(&[0x80, 0x01]);
        expected.extend(std::iter::repeat_n(b'0', PADDING_BYTES));
        assert_eq!(row.to_vec(), expected);
        assert!(row.len() > PADDING_BYTES);
    }

    #[test]
    fn the_padding_row_follows_schema_order() {
        let row = padding_row("pad String, tic UInt32, source UInt8").expect("a valid schema");
        assert_eq!(row.len(), 2 + PADDING_BYTES + 4 + 1);
        assert_eq!(&row[..2], &[0x80, 0x01]);
    }

    #[test]
    fn a_schema_without_a_string_column_is_rejected() {
        let err = padding_row("tic UInt32, source UInt8").expect_err("no String column");
        assert!(matches!(err, SchemaError::NoText), "{err}");
    }

    #[test]
    fn an_unencodable_column_names_itself_and_its_type() {
        let err = padding_row("tic UInt32, pad String, when DateTime")
            .expect_err("DateTime is not encoded here");
        let message = err.to_string();
        assert!(message.contains("when DateTime"), "{message}");
        assert!(message.contains("DateTime"), "{message}");
    }

    #[test]
    fn a_column_without_a_type_is_rejected() {
        let err = padding_row("tic, pad String").expect_err("no type on the first column");
        assert!(matches!(err, SchemaError::Shape { .. }), "{err}");
    }
}
