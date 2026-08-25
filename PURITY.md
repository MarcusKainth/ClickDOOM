# PURITY.md — What "DOOM runs in ClickHouse" means here

Committed before any implementation code, so the rules can't drift to fit
the code. Changes require human-owner approval (CODEOWNERS).

## The claim

The actual 1993 id Software DOOM engine (via the doomgeneric port), compiled
unmodified* to bare-metal RV32IM, executes on a CPU implemented **entirely in
ClickHouse SQL**, reading the shareware `doom1.wad`.

\* "Unmodified" = the platform layer (doomgeneric's `DG_*` hooks, crt0,
libc) is ours; the game engine sources are upstream, patched only where the
port requires (documented in `rom/patches/`).

## SQL must do

- All instruction fetch, decode, and execution.
- The register file, RAM, and every load/store — including the write-log
  and its merge into the `ram` table.
- All MMIO semantics: elastic time derivation, key-queue pop, framebuffer
  and palette state, console bytes.
- Frame readout: converting the 8bpp framebuffer + palette into the
  displayable form (RGB rows / ANSI string), inside a SELECT.

## The driver may ONLY

1. Loop: issue the batch statement and the frame-readout query.
2. Ferry input: INSERT raw key events into `input_queue`.
3. Blit output: print/store the frame bytes exactly as SQL produced them.
4. Housekeeping that computes nothing: create tables from `schema.sql`,
   load the ROM image into `ram`, record timings.

The driver must contain **zero game, CPU, or rendering logic**. Litmus test:
replacing the driver with a shell script of `clickhouse-client` calls and
`cat` must be conceivable without moving any logic into SQL, because the
logic is already there.

## Forbidden

- Executable UDFs, `python()`/`executable()` table functions, or any
  mechanism that delegates computation to a subprocess. (Click-V uses a UDF
  for host I/O; we deliberately go stricter — our I/O is tables.)
- Computation in the driver, however trivial it looks (no palette lookups,
  no key-repeat logic, no frame diffing).
- Precomputed answers: no tables of pre-decoded instructions generated
  outside ClickHouse, no imported traces. (Decoding the ROM *inside*
  ClickHouse into a decoded-instruction table is fine — that's SQL doing
  the work. Doing it in Python and inserting the result is not.)
- Wall-clock or host-environment dependence on any computation path.

## Explicitly allowed (pre-answering the HN thread)

- An external driver loop. Every SQL-DOOM project has one (DOOMQL's Python
  client, Click-V's clock inserts); a database has no event loop, and
  issuing queries is not computation.
- Batching many instructions per statement. The batch is executed by
  ClickHouse; statement granularity is an optimization, not a purity issue.
- Embedding the WAD in the ROM image. That's the program's data, in RAM,
  like any computer.
- Terminal rendering of the SQL-produced frame (the "monitor cable" is not
  part of the computer).

## Enforcement

- `scripts/check_purity.sh` greps `sqlcpu/`, `executor/`, and `driver/` for
  forbidden constructs (`executable`, `python(`, `now()`, `rand(`,
  `generateRandom`, subprocess use in the driver) and runs in CI on every PR.
- Reviewer checklist item in every PR (see PR template).
- Violations found later are `P0` bugs, not judgment calls.
