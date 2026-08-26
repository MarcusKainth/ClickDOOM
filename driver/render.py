"""Frame readout — issue #29. Three SQL expressions, all computation that
belongs entirely inside a SELECT (PURITY.md, "Frame readout: converting
the 8bpp framebuffer + palette into the displayable form (RGB rows / ANSI
string), inside a SELECT"):

  1. `frame_readout_sql()` -- reconstructs the raw `fb`/`palette` byte
     strings from word-addressed storage and inserts one `frames_out` row
     per commit (SPEC §5: "written by the render query on FRAME_COMMIT").
  2. `ansi_render_sql()` -- converts a `frames_out` row's `fb`/`palette`
     into a printable ANSI string the driver prints verbatim (PURITY.md:
     the driver may only "blit output: print/store the frame bytes exactly
     as SQL produced them").
  3. `ppm_render_sql()` -- converts a `frames_out` row's `fb`/`palette`
     into a complete binary PPM (P6) image, one String, for the driver to
     write to a file unmodified (issue #204: an image of the first frame
     for the sprint blog post -- ANSI escape codes aren't something to
     commit to git or embed in a post).

Builds SQL expression *text*, like sqlcpu/checkpoint.py and executor/
fold.py -- does not execute anything itself, does not read from any
particular table beyond the names/shapes documented per function.

## The fixture this is built and validated against

Originally built against a fixture (`fixture_schema.sql` in this
directory) matching sqlcpu's proposed persistence shape, since #160 (the
real persistence: `batch_commit` gaining six write-log columns,
`framebuffer`/`palette` tables) was filed and human-gated at the time --
same pattern #130 used before #145 landed the fold half. Confirmed the
fixture shape with `sqlcpu-2` before implementing (issue #29's plan
comment). **#160 has since ratified and landed** (`sqlcpu/schema.sql`),
matching the fixture byte-for-byte -- `refemu-2` independently confirmed
this module's two queries work unmodified against the real tables
(real-seeded `batch_commit`, `commit.py`'s real `fbpal_flush_sql()`,
this module's real `frame_readout_sql()`/`ansi_render_sql()`, reproducing
`fb_hash fe5d82c0f42d45f1`). The fixture stays in the tree as a fast,
isolated test path rather than requiring the full shared schema for every
test run.

## Why the word->bytes technique isn't imported from checkpoint.py

`sqlcpu/checkpoint.py`'s `word_array_hash()` already established the
technique this needs (`hex(reinterpretAsFixedString(toUInt32(w)))` per
word, `arrayStringConcat`, one `unhex()` -- avoids `arrayStringConcat`'s
silent embedded-null truncation on raw FixedStrings, per that module's own
docstring). But it bundles the technique with `xxHash64(...)` and doesn't
expose the intermediate bytes -- this module needs the *bytes themselves*
(to store in `frames_out.fb`/`.palette`), not a hash of them. Rather than
either reimplementing the technique blind or cross-scope-editing
`checkpoint.py` to factor it out (sqlcpu's file, not signed off for this
task), `region_bytes_sql()` below is the same one-line expression, cited
from `word_array_hash()`'s own documented technique, not independently
reinvented. A shared `words_to_bytes_sql()` primitive in `checkpoint.py`
that both `word_array_hash()` and this module call would be a clean, small
follow-up -- flagged, not silently worked around forever.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "sqlcpu"))
import checkpoint

DB = "clickdoom"

# SPEC §2: FRAMEBUFFER is 64,000 bytes (320x200, 8bpp), PALETTE is 768
# bytes (256 x RGB). Both divide by 4 exactly (sqlcpu's #130 finding,
# confirmed independently against DG_DrawFrame's word-store loop in
# rom/src/dg_hooks.c while reviewing #149) -- word storage, not byte.
FRAMEBUFFER_WORDS = 16_000
PALETTE_WORDS = 192
FB_WIDTH = 320
FB_HEIGHT = 200


def region_bytes_sql(words_expr: str) -> str:
    """Raw little-endian bytes from an Array(UInt32), address-ascending
    order -- see this module's docstring for why this one line is cited
    from checkpoint.py's word_array_hash() rather than imported or
    reinvented."""
    return (
        f"unhex(arrayStringConcat(arrayMap("
        f"w -> hex(reinterpretAsFixedString(toUInt32(w))), {words_expr})))"
    )


def frame_readout_sql(db: str = DB) -> str:
    """INSERT one `frames_out` row from the latest committed frame's
    `framebuffer`/`palette` word-table state (SPEC §5: frames_out is
    "written by the render query on FRAME_COMMIT").

    Reads `has_frame`/`frame_no`/`icount` from `batch_commit`, not
    `cpu_state` -- SPEC §5's `cpu_state` carries none of the three
    (confirmed against sqlcpu/schema.sql directly; only `batch_commit`
    does). `committed_icount` is exactly the committing batch's `icount`
    -- a batch stops the instant FRAME_COMMIT fires (SPEC §6), so there is
    no separate "commit instant" to derive.

    Correct to call once per commit, not once per instruction: the
    `framebuffer`/`palette` tables hold only the LATEST value per
    word_addr (ReplacingMergeTree), which is exactly the just-committed
    frame's content only because `DG_DrawFrame` writes both regions then
    calls FRAME_COMMIT as its last action, with nothing else in the ROM
    ever writing either region (verified against rom/src/dg_hooks.c while
    reviewing #149) -- read any later and a second `DG_DrawFrame` call may
    have already overwritten it.
    """
    fb_words = f"(SELECT groupArray(value) FROM (SELECT value FROM {db}.framebuffer FINAL ORDER BY word_addr))"
    pal_words = f"(SELECT groupArray(value) FROM (SELECT value FROM {db}.palette FINAL ORDER BY word_addr))"
    fb_bytes = region_bytes_sql(fb_words)
    pal_bytes = region_bytes_sql(pal_words)
    return f"""INSERT INTO {db}.frames_out (frame_no, committed_icount, fb, palette)
SELECT frame_no, icount, {fb_bytes} AS fb, {pal_bytes} AS palette
FROM (
    SELECT frame_no, icount
    FROM {db}.batch_commit
    WHERE has_frame = 1
    ORDER BY batch_id DESC
    LIMIT 1
)"""


def frame_readout_fb_hash_sql(db: str = DB) -> str:
    """The SPEC §7 fb_hash of the latest `frames_out` row -- the oracle
    check, not a second hash implementation (sqlcpu/checkpoint.py's
    fb_hash() used directly)."""
    fbhash_expr = checkpoint.fb_hash(framebuffer="fb", palette="palette")
    return f"""SELECT {checkpoint.hex64(fbhash_expr)} AS fbhash
FROM (SELECT fb, palette FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1)"""


def ansi_render_sql(db: str = DB, width: int = FB_WIDTH, height: int = FB_HEIGHT) -> str:
    """The latest `frames_out` row rendered as one printable ANSI string
    -- half-block truecolor (issue #29's plan comment): each terminal cell
    is two vertically-stacked pixels via U+2580 (upper half block), 24-bit
    truecolor escapes for foreground (top pixel) and background (bottom
    pixel) -- 320x200 pixels become 320x100 terminal cells at full
    horizontal resolution, the same technique tools like chafa/viu use for
    terminal image display. `driver/` has no prior convention to match
    (checked before choosing this -- only a README exists there).

    `width`/`height` default to SPEC §2's real 320x200 -- overridable only
    so a small synthetic case can be checked byte-for-byte without paying
    the full frame's cost; a caller pointing this at real `frames_out`
    data must leave them at the default (the query does not, and cannot,
    reshape `fb`'s actual byte layout to match an override).

    `px`/`pal_rgb` are computed once as plain array-typed columns (a
    two-way CROSS JOIN of two single-row subqueries, not a correlated
    subquery re-executed per pixel) and referenced directly inside the
    nested `arrayMap` lambdas below by name -- ClickHouse lambdas can read
    outer-scope columns, not only their own bound parameter, and computing
    the lookup structures once rather than per pixel reference is the same
    "compute a lookup structure once, not per reference" shape this
    project's cost model rewards elsewhere (ADR-0002's decode table, #80's
    per-batch-not-per-step accounting). Byte extraction: `fb`/`palette`
    are raw byte Strings (ClickHouse is 1-indexed for `substring`),
    `reinterpretAsUInt8` on a 1-byte `substring` recovers the numeric
    pixel index / palette channel value.
    """
    return f"""SELECT arrayStringConcat(
  arrayMap(
    r -> concat(
      arrayStringConcat(
        arrayMap(
          c -> concat(
            '\x1b[38;2;', toString(pal_rgb[px[r * 2 * {width} + c + 1] + 1].1),
            ';', toString(pal_rgb[px[r * 2 * {width} + c + 1] + 1].2),
            ';', toString(pal_rgb[px[r * 2 * {width} + c + 1] + 1].3), 'm',
            '\x1b[48;2;', toString(pal_rgb[px[(r * 2 + 1) * {width} + c + 1] + 1].1),
            ';', toString(pal_rgb[px[(r * 2 + 1) * {width} + c + 1] + 1].2),
            ';', toString(pal_rgb[px[(r * 2 + 1) * {width} + c + 1] + 1].3), 'm',
            '▀'
          ),
          range(0, {width})
        )
      ),
      '\x1b[0m'
    ),
    range(0, {height // 2})
  ),
  '\n'
) AS ansi_frame
FROM
  (SELECT arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, {width * height} + 1)) AS px
   FROM (SELECT fb FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1)) AS pixels,
  (SELECT arrayMap(i -> tuple(
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 1, 1)),
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 2, 1)),
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 3, 1))
    ), range(1, 257)) AS pal_rgb
   FROM (SELECT palette FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1)) AS palettes"""


def ppm_render_sql(db: str = DB, width: int = FB_WIDTH, height: int = FB_HEIGHT) -> str:
    """The latest `frames_out` row rendered as one complete binary PPM (P6)
    image, as a single String -- issue #204: the human owner wants an
    image of the first frame for the sprint blog post, and ANSI escape
    codes (`ansi_render_sql()` above) aren't something to commit to git or
    paste into a post.

    PURITY.md decides the shape: `ansi_render_sql()` already does the
    index-to-RGB palette lookup in SQL, with the driver only printing the
    result verbatim. This does exactly the same -- the driver's only job
    is to write these bytes to a path unmodified, never to touch a pixel
    or a palette entry. No palette lookup in Python; that would move
    computation into the driver and break the rule the whole project is
    built on.

    Binary PPM (P6) chosen because it needs no encoder and no dependency
    -- a five-token ASCII header (`P6\\n{width} {height}\\n255\\n`)
    followed by raw RGB triples, row-major, is the entire format, and SQL
    can build both halves with string concatenation alone. Converting to
    PNG for the actual blog post (`sips`/ImageMagick, one line) happens
    *outside* the purity boundary, on the PPM bytes this function already
    finished computing -- this function's job ends at valid PPM bytes,
    not at PNG.

    Same `px`/`pal_rgb` FROM clause as `ansi_render_sql()` above -- copied
    verbatim, not reimplemented: computed once as array-typed columns via
    the same two-way `CROSS JOIN` of two single-row subqueries, not a
    correlated subquery re-executed per pixel. Same reasoning too:
    computing a lookup structure once instead of per-reference is the
    cost model this project rewards everywhere else (ADR-0002's decode
    table, #80's per-batch-not-per-step accounting).

    Per-pixel RGB bytes go through the same hex-encode / `arrayStringConcat`
    / one-`unhex()` technique `region_bytes_sql()` above uses (cited from
    `checkpoint.py`'s `word_array_hash()`, see module docstring) --
    `arrayStringConcat` silently truncates the WHOLE result at the first
    embedded null byte across any array element, and an R/G/B channel
    byte being exactly 0x00 (black, one of the most common colors in
    DOOM's palette) is not an edge case here, it is most of a typical
    frame. Hex digits never contain a null byte, so each channel is
    encoded to 2 hex chars before the array-join, and `unhex()` once
    turns the whole 6-hex-chars-per-pixel string back into the actual 3
    raw bytes per pixel.

    The ASCII header is joined to the raw pixel bytes with a plain
    `concat()`, not `arrayStringConcat` -- `concat()` on two complete
    String values doesn't have the embedded-null problem described above.
    `checkpoint.py`'s own `bytes_hash()` already relies on exactly this
    for the real SPEC §7 `fb_hash` (`concat(framebuffer, palette)`, both
    full of null bytes in practice), verified correct all session against
    refemu's own hash on real data -- not a new assumption, the same one
    this codebase already depends on everywhere `fb_hash` is computed.
    """
    header = f"P6\n{width} {height}\n255\n"
    pixel_hex = (
        f"arrayStringConcat(arrayMap("
        f"i -> concat("
        f"hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].1))),"
        f"hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].2))),"
        f"hex(reinterpretAsFixedString(toUInt8(pal_rgb[px[i] + 1].3)))"
        f"), range(1, {width * height} + 1)))"
    )
    return f"""SELECT concat('{header}', unhex({pixel_hex})) AS ppm
FROM
  (SELECT arrayMap(i -> reinterpretAsUInt8(substring(fb, i, 1)), range(1, {width * height} + 1)) AS px
   FROM (SELECT fb FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1)) AS pixels,
  (SELECT arrayMap(i -> tuple(
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 1, 1)),
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 2, 1)),
      reinterpretAsUInt8(substring(palette, (i - 1) * 3 + 3, 1))
    ), range(1, 257)) AS pal_rgb
   FROM (SELECT palette FROM {db}.frames_out ORDER BY frame_no DESC LIMIT 1)) AS palettes"""
