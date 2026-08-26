# driver/

See CLAUDE.md for this workstream's charter and SPEC.md for its contracts.
Ownership is claimed via issue self-assignment.

## render.py (issue #29)

The frame readout: `frame_readout_sql()` reconstructs the raw
`fb`/`palette` byte strings from word-addressed FRAMEBUFFER/PALETTE
storage and inserts a `frames_out` row per commit; `ansi_render_sql()`
converts a committed frame into a printable half-block-truecolor ANSI
string, ready for the driver to print verbatim (PURITY.md: all of this is
SQL-side computation, the driver only blits the result).

**Built and validated against a fixture** (`fixture_schema.sql`), not the
real tables: #160 (the real FRAMEBUFFER/PALETTE persistence) is filed,
human-gated, not started. The fixture mirrors sqlcpu's proposed shape
exactly (confirmed with `sqlcpu-2`, see issue #29's plan comment) and gets
re-pointed once #160 lands, same pattern #130 used before #145.

Validated two ways, both against real evidence, not eyeballed:

1. `frame_readout_sql()` against **real refemu data** at the milestone
   icount (15,653,137) — `gen_frame_fixture.py` dumps refemu's actual
   FRAMEBUFFER/PALETTE bytes at that point (independently computing
   `fb_hash` itself, not trusting a cited number), `seed_frame_fixture.py`
   loads them into the fixture tables, and the readout reproduces
   `fb_hash fe5d82c0f42d45f1` — the same number #110/#160 cite — using
   `sqlcpu/checkpoint.py`'s real `fb_hash()`, never reimplemented.
2. `ansi_render_sql()` against a small hand-computed synthetic case (a
   2x2 image with known colors), checked byte-for-byte against an
   independently-computed expected escape sequence.

Run both: `driver/test_render.sh` (or `just test-render`).

### Why this is not wired to a real run

#160's persistence pipeline doesn't exist yet — `batch()` doesn't project
the framebuffer/palette write-log lanes, and `batch_commit` has no columns
for them (SPEC §5 needs an additive, human-ratified change first). This is
the query, proven correct against the real oracle number; it points at
real tables the moment #160 lands.
