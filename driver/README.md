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

**Originally built and validated against a fixture** (`fixture_schema.sql`),
before the real FRAMEBUFFER/PALETTE persistence (#160) ratified and landed
(#174). The fixture mirrors sqlcpu's proposed shape exactly (confirmed
with `sqlcpu-2`, see issue #29's plan comment), and #174's real
`sqlcpu/schema.sql` now matches it byte-for-byte — `refemu-2` independently
confirmed this module's queries work unmodified against the real,
persisted tables, reproducing `fb_hash fe5d82c0f42d45f1`. The fixture
stays in the tree as this module's own fast, isolated test path.

Validated two ways, both against real evidence, not eyeballed:

1. `frame_readout_sql()` against **real refemu data** at the milestone
   icount (15,393,136, as of #175's R_DrawColumn/R_DrawSpan unroll,
   `PINNED_HASH 9a6a47d0…`) — `gen_frame_fixture.py` dumps refemu's actual
   FRAMEBUFFER/PALETTE bytes at that point (independently computing
   `fb_hash` itself, not trusting a cited number), `seed_frame_fixture.py`
   loads them into the fixture tables, and the readout reproduces
   `fb_hash fe5d82c0f42d45f1` — the same number #110/#160 cite — using
   `sqlcpu/checkpoint.py`'s real `fb_hash()`, never reimplemented.
2. `ansi_render_sql()` against a small hand-computed synthetic case (a
   2x2 image with known colors), checked byte-for-byte against an
   independently-computed expected escape sequence.

Run both: `driver/test_render.sh` (or `just test-render`).

### Why this isn't wired into the milestone runner yet

#174 lands the persistence tables/columns, and `refemu-2` confirmed the
composition works against them — but `scripts/run_milestone.sh` (#144)
doesn't yet call `frame_readout_sql()`/`ansi_render_sql()` as part of its
loop. That wiring is a separate, small follow-up, not part of this PR's
scope.
