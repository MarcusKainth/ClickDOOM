//! Building the SPEC checkpoint hash expressions as SQL text.
//!
//! The trace format itself belongs to `clickdoom-spec`; this reproduces the
//! same contract in SQL rather than deciding it independently.

/// xxh64 over one or more raw byte-string expressions, concatenated in the
/// given order.
pub fn bytes_hash(byte_string_exprs: &[&str]) -> String {
    match byte_string_exprs {
        [one] => format!("xxHash64({one})"),
        many => format!("xxHash64(concat({}))", many.join(", ")),
    }
}

/// xxh64 over FRAMEBUFFER || PALETTE, each supplied as a raw byte string
/// already in address-ascending order. MMIO is excluded: live device state,
/// not something two engines need to agree on bit for bit.
pub fn fb_hash(framebuffer: &str, palette: &str) -> String {
    bytes_hash(&[framebuffer, palette])
}

/// 16-digit lowercase zero-padded hex, for a 64-bit hash column.
pub fn hex64(expr: &str) -> String {
    format!("lpad(lower(hex({expr})), 16, '0')")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_hash_of_one_expr_skips_concat() {
        assert_eq!(bytes_hash(&["a"]), "xxHash64(a)");
    }

    #[test]
    fn bytes_hash_of_several_exprs_concats_in_order() {
        assert_eq!(bytes_hash(&["a", "b"]), "xxHash64(concat(a, b))");
    }

    #[test]
    fn fb_hash_hashes_framebuffer_then_palette() {
        assert_eq!(fb_hash("fb", "pal"), "xxHash64(concat(fb, pal))");
    }

    #[test]
    fn hex64_pads_to_sixteen_digits() {
        assert_eq!(hex64("x"), "lpad(lower(hex(x)), 16, '0')");
    }
}
