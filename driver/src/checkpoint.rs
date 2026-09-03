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

/// 8-digit lowercase zero-padded hex, for `pc`.
pub fn hex32(expr: &str) -> String {
    format!("lpad(lower(hex({expr})), 8, '0')")
}

/// xxh64(pc || regs\[1..31\], each a 4-byte little-endian word,
/// register-index order). x0 is never hashed (always 0 by construction).
pub fn reg_hash(pc: &str, regs: &str) -> String {
    let mut words = vec![format!("reinterpretAsFixedString(toUInt32({pc}))")];
    words.extend((1..32).map(|i| format!("reinterpretAsFixedString(toUInt32({regs}[{i}]))")));
    format!("xxHash64(concat({}))", words.join(", "))
}

/// xxh64 over an `Array(UInt32)`, each word little-endian, in array order.
/// Goes through hex text rather than concatenating raw `FixedString`s:
/// `arrayStringConcat` truncates at an embedded null, which a real word can
/// contain; hex digits never do.
pub fn word_array_hash(words_expr: &str) -> String {
    format!(
        "xxHash64(unhex(arrayStringConcat(arrayMap(w -> hex(reinterpretAsFixedString(toUInt32(w))), {words_expr}))))"
    )
}

/// One SPEC checkpoint line as a single SQL string expression:
/// `icount<TAB>pc_hex<TAB>reghash_hex[<TAB>ramhash_hex<TAB>fbhash_hex]`.
/// `ramhash`/`fbhash` are expression names already bound elsewhere in the
/// same query; pass `None` for a plain-cadence line, both together for a
/// RAM_HASH_INTERVAL line.
pub fn format_checkpoint(
    icount: &str,
    pc: &str,
    reghash: &str,
    ram_and_fb_hash: Option<(&str, &str)>,
) -> String {
    let mut fields = vec![format!("toString({icount})"), hex32(pc), hex64(reghash)];
    if let Some((ramhash, fbhash)) = ram_and_fb_hash {
        fields.push(hex64(ramhash));
        fields.push(hex64(fbhash));
    }
    format!("concat({})", fields.join(", '\t', "))
}

/// The latest `cpu_state` row's full SPEC checkpoint line (all 5 fields),
/// for the `RAM_HASH_INTERVAL` cadence.
pub fn checkpoint_sql(db: &str) -> String {
    let ram_words = format!(
        "(SELECT groupArray(value) FROM (SELECT value FROM {db}.ram FINAL ORDER BY word_addr))"
    );
    let fb_words =
        crate::render::dense_words_sql(db, "framebuffer", crate::render::FRAMEBUFFER_WORDS);
    let pal_words = crate::render::dense_words_sql(db, "palette", crate::render::PALETTE_WORDS);
    let reghash_expr = reg_hash("pc", "regs");
    let ramhash_expr = word_array_hash(&ram_words);
    let fbhash_expr = fb_hash(
        &crate::render::region_bytes_sql(&fb_words),
        &crate::render::region_bytes_sql(&pal_words),
    );
    let line = format_checkpoint("icount", "pc", "reghash", Some(("ramhash", "fbhash")));
    format!(
        "SELECT {line}\nFROM (\n    \
         SELECT icount, pc, regs,\n           \
         {reghash_expr} AS reghash,\n           \
         {ramhash_expr} AS ramhash,\n           \
         {fbhash_expr} AS fbhash\n    \
         FROM (SELECT icount, pc, regs FROM {db}.cpu_state ORDER BY batch_id DESC LIMIT 1)\n)"
    )
}

/// Every register checkpoint one batch recorded, one SPEC checkpoint line
/// per row, in icount order.
///
/// The fold appends `(icount, pc, regs)` at each `CHECKPOINT_INTERVAL`
/// boundary it crosses and commits them with the batch. The hash is taken
/// here rather than inside the fold: it would otherwise be computed on
/// every step, since the fold disables short-circuit evaluation.
pub fn batch_checkpoints_sql(db: &str, batch_id: u64) -> String {
    let regs = clickdoom_executor::fold::CHECKPOINT_REGS;
    let reghash_expr = reg_hash("pc", "regs");
    let line = format_checkpoint("icount", "pc", "reghash", None);
    format!(
        "SELECT {line}\nFROM (\n    \
         SELECT cp_icount[n] AS icount, cp_pc[n] AS pc,\n           \
         arraySlice(cp_regs, (n - 1) * {regs} + 1, {regs}) AS regs,\n           \
         {reghash_expr} AS reghash\n    \
         FROM (\n        \
         SELECT cp_icount, cp_pc, cp_regs, arrayJoin(arrayEnumerate(cp_icount)) AS n\n        \
         FROM {db}.batch_commit\n        \
         WHERE batch_id = {batch_id}\n    \
         )\n)\nORDER BY icount"
    )
}

/// The latest `cpu_state` row's cheap register-only checkpoint line
/// (icount/pc/reghash), for the `CHECKPOINT_INTERVAL` cadence.
pub fn reg_checkpoint_sql(db: &str) -> String {
    let reghash_expr = reg_hash("pc", "regs");
    let line = format_checkpoint("icount", "pc", "reghash", None);
    format!(
        "SELECT {line}\nFROM (\n    \
         SELECT icount, pc,\n           \
         {reghash_expr} AS reghash\n    \
         FROM (SELECT icount, pc, regs FROM {db}.cpu_state ORDER BY batch_id DESC LIMIT 1)\n)"
    )
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
