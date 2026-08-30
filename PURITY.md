# PURITY.md — What "DOOM runs in ClickHouse" means here

Committed before any implementation code, so the rules can't drift to fit the
code. Changes require maintainer approval (CODEOWNERS).

The rules below are numbered `PUR-1` to `PUR-12` and are cited by number
everywhere else: in the pull request template, in issue forms, and in
`scripts/check_purity.sh`'s error messages. Touching one is not automatically
wrong. The change then has to say how the property still holds, and citing the
number is the reviewable form of that.

## The claim

The actual 1993 id Software DOOM engine (via the doomgeneric port), compiled
unmodified* to bare-metal RV32IM, executes on a CPU implemented **entirely in
ClickHouse SQL**, reading the shareware `doom1.wad`.

\* "Unmodified" = the platform layer (doomgeneric's `DG_*` hooks, crt0,
libc) is ours; the game engine sources are upstream, patched only where the
port requires (documented in `rom/patches/`).

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
  doing the work. Doing it in Python and inserting the result is not.
- **PUR-12.** Wall-clock or host-environment dependence on any computation
  path.

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
| PUR-9 | `check_purity.sh` | `sqlcpu/` `executor/` `driver/` `scripts/` |
| PUR-10 | `check_purity.sh` | `driver/` |
| PUR-11 | Review only. No textual signature | |
| PUR-12 | `check_purity.sh` | `sqlcpu/` `executor/` `driver/` `scripts/` |

A hit that is genuinely benign is annotated on the line with
`purity-ok: <reason>`, which the script honours. An annotation is a claim a
reviewer can check, so it states why the line is outside the rule rather than
that it is.

The pull request template asks which rules a change touches. Violations found
later are `P0` bugs, not judgment calls.
