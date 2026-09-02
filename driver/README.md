# driver/

The client side: `clickdoom`, the Rust binary that ticks the batch statement,
ferries key events in, and blits whatever frame SQL produced. `src/render.rs`
is the frame-readout SQL that produces it, and `src/frames.rs` is the blit:
it runs the PPM query and writes the bytes back out unchanged.

`PURITY.md` draws the line here, as PUR-5 to PUR-8 and PUR-10. The driver loops,
ferries key events in, blits output, and does housekeeping that computes
nothing. Frame readout is computation, so it is a SQL query that lives here and
runs SQL-side.

## Layout

`src/emulation/` holds what is specific to running the RV32IM CPU in SQL: the
ROM load, the reset seed, the text-region decode, the pre-flight gates, the
resumable batch loop and the differential run against the reference emulator.

Everything beside it names no instruction and no register. `src/client.rs` is
the connection, `src/sql.rs` splits a multi-statement string for the HTTP
interface, `src/checkpoint.rs` and `src/render.rs` build hash and readout SQL
text, `src/frames.rs` writes a frame file, `src/stats.rs` is the progress
line, and `src/bench/` is the throughput harness.

## Progress reporting

A run prints a `key=value` line to stderr at most once a second, alongside its
per-batch line:

    # stats elapsed=24.1s instr=100000 instr_per_sec=4423.3 instr_per_sec_mean=4146.4 batches=5 frames=0
    # stats final elapsed=24.1s instr=100000 instr_per_sec_mean=4146.4 batches=5 frames=0

`instr_per_sec` covers the window since the previous line and
`instr_per_sec_mean` the run so far, so a slowdown shows in the first field
before it moves the second. The counts start where the run resumed, so they
describe this process rather than every process that has run this database.

These are progress numbers off a shared machine, not a throughput
measurement. `docs/benchmarks.md` says where a number that can be compared to
another number comes from.

## The command line

`clickdoom` has one namespace per execution mode, and `src/cli/` has one
module per namespace.

`clickdoom emulation` runs the CPU in SQL: `ping`, `load-rom`, `bootstrap`,
`decode`, `render`, `preflight`, `run`, `diff` and `bench`. Each takes the
same connection flags, `--host`, `--port`, `--user`, `--database` and
`--password`, and the password falls back to `$CLICKHOUSE_PASSWORD` so it
never has to appear in `ps`.

`clickdoom native` names the mode that runs DOOM's own simulation and
renderer as SQL. It has no subcommands, so invoking it reports a usage error
and exits 2. `clickdoom native --help` describes the namespace.

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

    make test

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
6. The file `clickdoom emulation run --frame-dir` writes for a committed
   frame, byte for byte against what the same query returns, and named after
   the frame.

The frame hash is defined over the indexed representation and PPM over
expanded RGB, so the two share no single value. Check 3 is what ties them
together: one frame, two representations, each computed independently.
