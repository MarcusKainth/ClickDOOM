//! Frame readout: converting the 8bpp framebuffer and palette into the
//! displayable form, entirely inside a `SELECT`.
//!
//! This builds SQL expression text, like [`crate::checkpoint`]. It does not
//! execute anything itself, and it does not decide a pixel's color: the
//! driver's job downstream of these functions is to blit the bytes SQL
//! produced, unmodified.

use crate::checkpoint::{fb_hash, hex64};

/// SPEC's FRAMEBUFFER is 64,000 bytes (320x200, 8bpp); PALETTE is 768 bytes
/// (256 x RGB). Both divide by 4 exactly: word storage, not byte.
pub const FRAMEBUFFER_WORDS: u32 = clickdoom_spec::FRAMEBUFFER_SIZE / 4;
pub const PALETTE_WORDS: u32 = clickdoom_spec::PALETTE_SIZE / 4;
pub const FB_WIDTH: u32 = 320;
pub const FB_HEIGHT: u32 = 200;

/// Raw little-endian bytes from an `Array(UInt32)`, address-ascending
/// order.
pub fn region_bytes_sql(words_expr: &str) -> String {
    format!(
        "unhex(arrayStringConcat(arrayMap(w -> hex(reinterpretAsFixedString(toUInt32(w))), {words_expr})))"
    )
}

/// A dense `Array(UInt32)` over `[0, n_words)` for `framebuffer`/`palette`:
/// tables that start with zero rows and gain one per address only once the
/// ROM's first store there happens. A bare `groupArray` over a range with
/// an unwritten word comes back short, not zero-filled, which shifts every
/// later byte's alignment. `LEFT JOIN` against a `numbers(n_words)` address
/// domain, coalescing a missing match to 0, closes the gap.
pub fn dense_words_sql(db: &str, table: &str, n_words: u32) -> String {
    format!(
        "(SELECT groupArray(value) FROM (\
         SELECT coalesce(t.value, 0) AS value \
         FROM (SELECT number AS word_addr FROM numbers({n_words})) n \
         LEFT JOIN (SELECT word_addr, value FROM {db}.{table} FINAL) t \
         ON n.word_addr = t.word_addr \
         ORDER BY n.word_addr\
         ))"
    )
}

/// Inserts one `frames_out` row from the latest committed frame's
/// `framebuffer`/`palette` word-table state. Correct to call once per
/// commit: `framebuffer`/`palette` hold only the latest value per
/// `word_addr`, which is the just-committed frame's content only right
/// after the commit fires.
pub fn frame_readout_sql(db: &str) -> String {
    let fb_bytes = region_bytes_sql(&dense_words_sql(db, "framebuffer", FRAMEBUFFER_WORDS));
    let pal_bytes = region_bytes_sql(&dense_words_sql(db, "palette", PALETTE_WORDS));
    format!(
        "INSERT INTO {db}.frames_out (frame_no, committed_icount, fb, palette)\n\
         SELECT frame_no, icount, {fb_bytes} AS fb, {pal_bytes} AS palette\n\
         FROM (\n    \
             SELECT frame_no, icount\n    \
             FROM {db}.batch_commit\n    \
             WHERE has_frame = 1\n    \
             ORDER BY batch_id DESC\n    \
             LIMIT 1\n\
         )"
    )
}

/// The SPEC `fb_hash` of the latest `frames_out` row: the oracle check, not
/// a second hash implementation.
pub fn frame_readout_fb_hash_sql(db: &str) -> String {
    let fbhash_expr = fb_hash("fb", "palette");
    format!(
        "SELECT {} AS fbhash\nFROM (SELECT fb, palette FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1)",
        hex64(&fbhash_expr)
    )
}

/// The latest `frames_out` row rendered as one printable ANSI string: half-
/// block truecolor, two vertically stacked pixels per terminal cell via
/// U+2580 (upper half block), 24-bit truecolor escapes for foreground (top
/// pixel) and background (bottom pixel). `width`/`height` default to SPEC's
/// real 320x200; a caller pointing this at real `frames_out` data must
/// leave them there, since the query does not reshape `fb`'s actual byte
/// layout to match an override.
pub fn ansi_render_sql(db: &str, width: u32, height: u32) -> String {
    let half_height = height / 2;
    let area = width * height;
    format!(
        "SELECT arrayStringConcat(\n  \
             arrayMap(\n    \
                 r -> concat(\n      \
                     arrayStringConcat(\n        \
                         arrayMap(\n          \
                             c -> concat(\n            \
                                 '\x1b[38;2;', toString(pal_rgb[px[r * 2 * {width} + c + 1] + 1].1),\n            \
                                 ';', toString(pal_rgb[px[r * 2 * {width} + c + 1] + 1].2),\n            \
                                 ';', toString(pal_rgb[px[r * 2 * {width} + c + 1] + 1].3), 'm',\n            \
                                 '\x1b[48;2;', toString(pal_rgb[px[(r * 2 + 1) * {width} + c + 1] + 1].1),\n            \
                                 ';', toString(pal_rgb[px[(r * 2 + 1) * {width} + c + 1] + 1].2),\n            \
                                 ';', toString(pal_rgb[px[(r * 2 + 1) * {width} + c + 1] + 1].3), 'm',\n            \
                                 '\u{2580}'\n          \
                             ),\n          \
                             range(0, {width})\n        \
                         )\n      \
                     ),\n      \
                     '\x1b[0m'\n    \
                 ),\n    \
                 range(0, {half_height})\n  \
             ),\n  \
             '\n'\n\
         ) AS ansi_frame\n\
         FROM\n  \
             (SELECT arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, {area} + 1)) AS px\n   \
              FROM (SELECT fb FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1)) AS pixels,\n  \
             (SELECT arrayMap(i -> tuple(\n      \
                 reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 1, 1)),\n      \
                 reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 2, 1)),\n      \
                 reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 3, 1))\n    \
             ), range(1, 257)) AS pal_rgb\n   \
              FROM (SELECT palette FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1)) AS palettes"
    )
}

/// The latest `frames_out` row rendered as one complete binary PPM (P6)
/// image, as a single string.
pub fn ppm_render_sql(db: &str, width: u32, height: u32) -> String {
    let latest =
        |column| format!("SELECT {column} FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1");
    ppm_sql_over(&latest("fb"), &latest("palette"), width, height)
}

/// One complete binary PPM (P6) image, as a single string, from a source of
/// `fb` bytes and a source of `palette` bytes.
///
/// Binary PPM needs no encoder and no dependency: a five-token ASCII header
/// followed by raw RGB triples, row-major, is the entire format, and SQL
/// builds both halves with string concatenation alone. Each source is a
/// `SELECT` returning the one column it is named for and one row.
pub fn ppm_sql_over(fb: &str, palette: &str, width: u32, height: u32) -> String {
    let header = format!("P6\n{width} {height}\n255\n");
    let area = width * height;
    let pixel_hex = format!(
        "arrayStringConcat(arrayMap(\
         i -> concat(\
         hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].1))),\
         hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].2))),\
         hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].3)))\
         ), range(1, {area} + 1)))"
    );
    format!(
        "SELECT concat('{header}', unhex({pixel_hex})) AS ppm\n\
         FROM\n  \
             (SELECT arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, {area} + 1)) AS px\n   \
              FROM ({fb})) AS pixels,\n  \
             (SELECT arrayMap(i -> tuple(\n      \
                 reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 1, 1)),\n      \
                 reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 2, 1)),\n      \
                 reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 3, 1))\n    \
             ), range(1, 257)) AS pal_rgb\n   \
              FROM ({palette})) AS palettes"
    )
}
