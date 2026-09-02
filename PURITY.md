# PURITY.md — What "DOOM runs in ClickHouse" means here

Committed before any implementation code, so the rules can't drift to fit the
code. Changes require maintainer approval (CODEOWNERS).

The rules below are numbered `PUR-1` to `PUR-15` and are cited by number
everywhere else: in the pull request template, in issue forms, and in
`scripts/check_purity.sh`'s error messages. Touching one is not automatically
wrong. The change then has to say how the property still holds, and citing the
number is the reviewable form of that.

## The claim

ClickDOOM has two modes, and each makes its own claim.

Emulation mode: the actual 1993 id Software DOOM engine (via the doomgeneric
port), compiled unmodified* to bare-metal RV32IM, executes on a CPU implemented
**entirely in ClickHouse SQL**, reading the shareware `doom1.wad`.

Native mode: DOOM's own game simulation and renderer, written as ClickHouse
SQL, run the shareware `doom1.wad` and produce, tic for tic and pixel for
pixel, the state and the frames the real engine produces. The real engine
running in the reference emulator is the oracle that says so.

\* "Unmodified" = the platform layer (doomgeneric's `DG_*` hooks, crt0,
libc) is ours; the game engine sources are upstream, patched only where the
port requires (documented in `rom/patches/`).

## Which rules apply where

PUR-1 to PUR-3 describe the CPU and apply to emulation mode. PUR-4 to PUR-12
apply to both modes. PUR-13 to PUR-15 describe native mode. `SPEC.md` is
emulation mode's contract and `NATIVE.md` is native mode's; neither applies to
the other mode.

## SQL must do

- **PUR-1.** All instruction fetch, decode, and execution.
- **PUR-2.** The register file, RAM, and every load/store, including the
  write-log and its merge into the `ram` table.
- **PUR-3.** All MMIO semantics: elastic time derivation, key-queue pop,
  framebuffer and palette state, console bytes.
- **PUR-4.** Frame readout: converting the 8bpp framebuffer and palette into
  the displayable form (RGB rows, ANSI string), inside a SELECT.

## The driver may only

- **PUR-5.** Loop: issue the batch statement and the frame-readout query.
- **PUR-6.** Ferry input: INSERT raw key events into `input_queue`.
- **PUR-7.** Blit output: print or store the frame bytes exactly as SQL
  produced them.
- **PUR-8.** Housekeeping that computes nothing: create tables from
  `schema.sql`, load the ROM image into `ram`, record timings.

The driver contains zero game, CPU, or rendering logic. Litmus test: replacing
the driver with a shell script of `clickhouse-client` calls and `cat` must be
conceivable without moving any logic into SQL, because the logic is already
there.

## Forbidden

- **PUR-9.** Executable UDFs, `python()` or `executable()` table functions, or
  any mechanism that delegates computation to a subprocess. (Click-V uses a UDF
  for host I/O. This project goes stricter and its I/O is tables.)
- **PUR-10.** Computation in the driver, however trivial it looks. No palette
  lookups, no key-repeat logic, no frame diffing.
- **PUR-11.** Precomputed answers: no tables of pre-decoded instructions
  generated outside ClickHouse, no imported traces. Decoding the ROM *inside*
  ClickHouse into a decoded-instruction table is fine, because that is SQL
  doing the work. Decoding it in the driver and inserting the result is not.
- **PUR-12.** Wall-clock or host-environment dependence on any computation
  path.

## Native mode

- **PUR-13.** All game simulation runs in SQL: the thinkers in the engine's
  order, collision, line-of-sight, the random number draws, sector specials,
  the player's tic including building its command from key state, and the
  status bar, message and menu tickers.
- **PUR-14.** All rendering runs in SQL: texture composition, BSP traversal,
  wall, plane and sprite drawing with the engine's clipping, the status bar
  and message drawing, the palette choice and the screen melt.
- **PUR-15.** The driver may insert the WAD's lumps as raw bytes and the
  engine's constant tables generated from the vendored engine source by the
  checked-in generator, stream one input row per tic, poll results, and blit
  frame bytes as SQL produced them. Everything derived from a lump is derived
  in SQL. A constant table that cannot be regenerated from `rom/vendor/` by
  that generator is a PUR-11 violation.

## Explicitly allowed

These are carve-outs from the rules above, not rules of their own.

- An external driver loop. Every SQL-DOOM project has one (DOOMQL's Python
  client, Click-V's clock inserts). A database has no event loop, and issuing
  queries is not computation.
- Batching many instructions per statement. The batch is executed by
  ClickHouse, so statement granularity is an optimization and not a purity
  question.
- Embedding the WAD in the ROM image. That is the program's data, in RAM, like
  any computer.
- Terminal rendering of the SQL-produced frame. The monitor cable is not part
  of the computer.
- Recording wall-clock time in a benchmark harness. A harness measures the
  system from outside; no emulator result depends on what it reads. Annotate
  each use with `purity-ok:` and say so.
- Pacing native mode to 35 Hz and printing a progress line. The clock decides
  when the driver sends the next row and what it reports; no value the
  simulation or the renderer computes depends on it. Annotate each read with
  `purity-ok:` and say so.
- Reinterpreting bytes the server produced as the words a window wants. The
  frame row already holds the RGB words; the driver hands them over as they
  are.

## Enforcement

`scripts/check_purity.sh` runs in CI on every pull request. It greps, so it
reaches the rules that have a textual signature and no others. Which rules
those are is stated here rather than left to be inferred, because a gate whose
documented reach is wider than its implementation lets a reviewer rely on a
guarantee nobody made.

| Rule | How it is enforced | Scanned |
|---|---|---|
| PUR-1 to PUR-4 | Review only. No textual signature | |
| PUR-5 to PUR-8 | Review only. No textual signature | |
| PUR-9 | `check_purity.sh` | `sqlcpu/` `executor/` `driver/` `native/` `scripts/` |
| PUR-10 | Review only. No textual signature | |
| PUR-11 | Review only. No textual signature | |
| PUR-12 | `check_purity.sh` | `sqlcpu/` `executor/` `driver/` `native/` `scripts/` |
| PUR-13 to PUR-15 | Review only, plus the parity gates: a simulation or renderer that computed outside SQL would still have to match the engine, and `make native-smoke` and `make native-parity` check that it does. Neither gate can tell where a value was computed | |

A hit that is genuinely benign is annotated on the line with
`purity-ok: <reason>`, which the script honours. An annotation is a claim a
reviewer can check, so it states why the line is outside the rule rather than
that it is.

The pull request template asks which rules a change touches. Violations found
later are `P0` bugs, not judgment calls.
