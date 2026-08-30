# driver/

The client side: the loop that ticks the batch statement and blits whatever
frame SQL produced, and the frame-readout SQL that produces it.

`PURITY.md` draws the line here, as PUR-5 to PUR-8 and PUR-10. The driver loops,
ferries key events in, blits output, and does housekeeping that computes
nothing. Frame readout is computation, so it is a SQL query that lives here and
runs SQL-side.

## Frame readout

The readout reconstructs the raw framebuffer and palette bytes from
word-addressed storage and records one row per committed frame. Two render
forms sit on top of it: a half-block truecolor ANSI string the driver prints
verbatim, and a binary PPM the driver writes to a file unmodified. An image
file is what you want for anything outside a terminal.

There is also a hash-only readout, for a run that wants the frame check without
carrying the bytes. `scripts/run_milestone.sh` uses both, calling into this
module rather than reimplementing either.

All of it is SQL text generated here and evaluated in the database. Nothing in
this directory computes a pixel.

## Tests

    make test-render

Needs a live ClickHouse, which the target starts. On a host without
`clickhouse-client` on `PATH`, pass one:

    make test-render CH_CLIENT="docker exec -i clickdoom-ch clickhouse-client"

Runs against a throwaway `driver_render_test_<pid>` database, never the shared
one. None of these checks is eyeballed:

1. The readout against real reference-emulator data at the milestone icount,
   reproducing `fb_hash fe5d82c0f42d45f1`. The check is computed by `sqlcpu/`'s
   own frame hash, never reimplemented here.
2. The PPM render of that same frame, byte-exact against an independent Python
   re-derivation from the raw framebuffer and palette.
3. The ANSI render of a hand-computed 2x2 case, byte-exact against an
   independently computed escape sequence.
4. The PPM render of that same 2x2 case.

The frame hash is defined over the indexed representation and PPM over expanded
RGB, so the two share no single value. Check 2 is what ties them together: one
frame, two representations, each computed independently.

## Fixtures

The module has a fast test path that needs no ROM run: a fixture schema
mirroring the real table shapes, plus generators that dump the reference
emulator's framebuffer and palette at a target icount and load them in. Dense
and sparse storage each have a pair.
