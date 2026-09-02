# ClickDOOM

**Knee-Deep in the Rows**: DOOM inside ClickHouse, two ways. The actual 1993
engine running on a RISC-V CPU implemented in SQL, and the engine's own
simulation and renderer rewritten as SQL, checked against the first, tic by
tic and pixel by pixel.

[![ci](https://github.com/MarcusKainth/ClickDOOM/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/MarcusKainth/ClickDOOM/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-Apache--2.0%20%2F%20GPL--2.0-blue)](LICENSING.md)

ClickDOOM has two modes.

**Emulation mode** executes the real id Software engine: DOOM's C source, via
the doomgeneric port, is compiled unmodified to bare-metal RV32IM, and that
binary runs instruction by instruction on a CPU emulator built entirely in
ClickHouse SQL: fetch, decode, execute, registers, RAM, MMIO, all of it. The
lineage is [Click-V](https://github.com/SpencerTorres/Click-V), which proved a
RISC-V CPU can live inside ClickHouse; ClickDOOM builds a faster
batch-execution engine and points it at DOOM. It is exact and it is slow: about
5,000 instructions per second, which puts the `demo3` timedemo at two weeks.

**Native mode** is the genre this project once distanced itself from, done to
a different standard. Excellent Doom-likes in SQL exist (DOOMHouse, DOOMQL,
DuckDB-DOOM). Native mode is not a Doom-like: it is DOOM's own tic simulation
and its own renderer, function for function, written as ClickHouse SQL and run
as two statements that stay open for a whole session, one row per tic. It plays
`demo3` at the engine's 35 Hz in a window, and it is held to the real engine:
the reference emulator reads the engine's game state out of RAM at every frame
and hashes every frame it draws, and native mode has to produce the same state
row and the same 64,000 bytes. [NATIVE.md](NATIVE.md) is its contract.

What counts as "in SQL" for each mode is defined in [PURITY.md](PURITY.md),
committed before the first line of code.

![A DOOM screenshot: the player firing a shotgun at a red imp in a stone corridor, a second enemy to the right, muzzle flash lighting the scene, status bar showing 10 ammo and 100% health](docs/blog/images/frame220-demo3.png)

Frame 220 of `-timedemo demo3`, after 221,639,724 instructions. Every pixel
computed inside ClickHouse, read back out by a SQL query, and hashed to
`aa27f0470c7c5f3a` — the same value the reference emulator produces from its
own independent run.

## Build log

**[Turning an optimisation off made it faster](docs/blog/2026-09-02-turning-an-optimisation-off.md)**
· 2 September 2026 — switching off ClickHouse's short-circuit evaluation is
worth 17%, our own benchmark had been understating us by 18%, and six figures
is now ruled out with a number rather than a feeling.

**[The sprint that mostly said no](docs/blog/2026-08-26-the-sprint-that-mostly-said-no.md)**
· 26 August 2026 — how `demo3` went from 44 days to 15, the five
optimisations that failed, and why the failures were worth more than the
wins. Written by the team lead, who is also an AI agent.

## How it works

Emulation mode is five workstreams connected by two contracts, both written
down in [SPEC.md](SPEC.md). Native mode adds one crate and one document and is
described after them.

**rom** builds DOOM into the thing the CPU actually runs: doomgeneric
compiled to a bare-metal RV32IM binary, with the shareware `doom1.wad`
embedded in the image, crt0 and libc shims standing in for an OS that
isn't there.

**refemu** is a Rust RV32IM interpreter that runs the same ROM and
serves as the oracle — the known-good trace that the SQL implementation is
checked against, instruction by instruction (SPEC §7).

**sqlcpu** and **executor** are the CPU itself: instruction decode and
execute as ClickHouse SQL, driven by an `arrayFold` batch loop with
write-log memory and MMIO plumbing, so a batch of instructions commits to
RAM in one statement rather than one round trip each.

**driver** is deliberately dumb — a Rust loop that ticks the batch
statement, feeds key events in, and blits whatever frame SQL produced.
[PURITY.md](PURITY.md) is the enforceable boundary here: no game logic,
no CPU logic, no rendering logic reaches the driver. Frame readout — 8bpp
framebuffer plus palette turned into displayable rows — is itself a SQL
query (`driver/`, computed SQL-side, run by the driver only as a `SELECT`).

**native** holds native mode: the WAD's lumps loaded as raw bytes, the
engine's constant tables generated from the vendored source, and SQL that
decodes the level, composes the textures, runs the tic and draws the frame.
`refemu` gains a probe that writes the real engine's game state as rows of the
same shape, so `clickdoom native diff` reports the first tic and field on which
the two disagree, and every frame is compared by hash.

Two contracts hold this together: the ROM boundary (rom ↔ refemu ↔ sqlcpu
all execute the identical binary) and the trace format differential
testing is checked against (SPEC §7). Everything else is workstream-local.

## Quick start

```sh
make up        # pinned ClickHouse via docker compose
make build-rom # reproducible DOOM ROM (dockerized rv32 toolchain)
make test      # every suite, riscv-tests inside the database included
make smoke     # 100,000-instruction differential run against the oracle
```

`make lint` runs the purity check plus the linters, and is what CI runs on
every pull request. `make help` lists every target.

## Contributing

The most useful contribution is one that falsifies the claim above: a case
where the SQL CPU and the reference emulator disagree on the same ROM, or a
place where computation has left SQL.

[CONTRIBUTING.md](CONTRIBUTING.md) is the entry point.
[DEVELOPING.md](DEVELOPING.md) has the build, test and benchmark mechanics,
[PURITY.md](PURITY.md) states the rules the project is held to, and
[AGENTS.md](AGENTS.md) is what a coding agent should read first.

Much of this repository was written by AI agents. [AI_POLICY.md](AI_POLICY.md)
says what that changes about a contribution, and what it does not.

## Credits & prior art

- [Click-V](https://github.com/SpencerTorres/Click-V) — RISC-V in
  ClickHouse SQL; proof the premise works.
- [DOOMHouse](https://github.com/arniwesth/DoomHouse), DOOMQL (CedarDB),
  DuckDB-DOOM — the SQL Doom-like renaissance this project answers.
- [doomgeneric](https://github.com/ozkl/doomgeneric) and the id Software
  DOOM source it carries.

## License

Copyright 2026 Marcus Kainth.

The emulator is Apache-2.0. `rom/` builds the DOOM binary and is
GPL-2.0-or-later. The embedded shareware `doom1.wad` is under id Software's
own distribution terms and no commercial WADs go in this repo.
[LICENSING.md](LICENSING.md) has the boundary in full.
