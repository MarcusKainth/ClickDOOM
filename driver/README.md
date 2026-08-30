# driver/

The client side: `clickdoom`, the Rust binary that ticks the batch statement,
ferries key events in, and blits whatever frame SQL produced. `src/render.rs`
is the frame-readout SQL that produces it.

`PURITY.md` draws the line here, as PUR-5 to PUR-8 and PUR-10. The driver loops,
ferries key events in, blits output, and does housekeeping that computes
nothing. Frame readout is computation, so it is a SQL query that lives here and
runs SQL-side.

`render.py` is the Python module `render.rs` replaced. It stays in the tree
solely as `rom/bench/canonical_throughput/seed_snapshot.py`'s dependency,
which is unrelated to `clickdoom` and not covered by anything below.

## Frame readout

The readout reconstructs the raw framebuffer and palette bytes from
word-addressed storage and records one row per committed frame. Two render
forms sit on top of it: a half-block truecolor ANSI string, and a binary PPM.
An image file is what you want for anything outside a terminal.

There is also a hash-only readout, for a check that wants the frame comparison
without carrying the bytes. `src/checkpoint.rs` calls into `render.rs` for
both rather than reimplementing either.

All of it is SQL text generated here and evaluated in the database. Nothing in
this directory computes a pixel.

## Tests

`tests/render_golden.rs` proves the generated SQL text is byte-identical to a
known-correct reference; it needs no ClickHouse.

    make test-render

runs `tests/render_live.rs`, which executes every render query for real
against a live ClickHouse. None of its checks are eyeballed:

1. A genuinely sparse `framebuffer`/`palette` table reconstructs as a
   zero-filled dense region, not a shorter, misaligned one. A bare
   `groupArray`/`FINAL` read, included as a negative control, is shown to get
   this wrong on the same data.
2. The readout against a real `refemu`-captured frame reproduces the exact
   `fb_hash` that capture recorded.
3. The PPM render of that same frame, byte-exact against an independent
   re-derivation from the frame's own raw pixels and palette.
4. The ANSI render of a hand-computed 2x2 case, byte-exact against an
   independently computed escape sequence.
5. The PPM render of that same 2x2 case.

The frame hash is defined over the indexed representation and PPM over
expanded RGB, so the two share no single value. Check 3 is what ties them
together: one frame, two representations, each computed independently.
