# Developing ClickDOOM

Maintainer and returning-contributor reference: the build, test and benchmark
mechanics. [`CONTRIBUTING.md`](CONTRIBUTING.md) is the entry point and covers
policy; this file covers how things run.

## Prerequisites

| Tool | Why |
|---|---|
| Docker | ClickHouse and the rv32 toolchain both run in containers |
| `make` | Every task. `make help` lists them |
| `cargo` | The Rust workspace. `rust-toolchain.toml` pins the version |
| `shellcheck` | `make shellcheck` |
| `clang-format` | `make clang-format`, over `rom/src/` |
| `actionlint`, `zizmor` | Workflow linting, both in `make lint` |

Rust comes from `rust-toolchain.toml`, so the first
`cargo` call in a fresh checkout installs the pinned version and `make lint`
speaks the same `rustfmt` and `clippy` as CI. The rv32 toolchain is pinned to
xPack `riscv-none-elf-gcc` 15.2.0-1 and built from `rom/toolchain/Dockerfile`;
you never install it yourself.

## Targets

`make help` is the inventory. `make gates` is the pull request bar: `lint`, the
ROM hash check, every suite in `make test`, and `smoke`. Those are the jobs
`ci.yml` runs on a pull request. It takes upwards of ten minutes, most of it the
ROM build and the smoke diff.

`lint` also runs `check-adr` and `actionlint`, which have no CI job of their own.

The benches, `fuzz`, the milestone targets and the nightly deep-diff sit outside
`gates`, by cost or by what they need. A timing run needs a quiet machine, and
the deep-diff takes hours.

What `make help` does not say:

- **`make up` is a prerequisite of most test and bench targets**, so you rarely
  run it yourself. It waits for the container to report healthy.
- **`test`, `diff` and the bench need a live ClickHouse.** `lint`,
  `build-rom` and a bare `cargo test --workspace` do not.
- **`check-rom-hash` links the ROM twice.** `rom/PINNED_HASH` pins the flat
  binary. The ELF is pinned by linking it a second time and comparing the two
  files, because `objcopy` drops the symbol and string tables on the way to
  the flat binary and the hash check never sees them.
- **Targets are not parallel-safe.** Most share one container, and the
  compiled-expression cache is server-global, so two timing runs at once
  measure each other. Do not pass `-j`.
- **Every `clickhouse-client`-driving target closes stdin.** `clickhouse-client`
  blocks forever on an INSERT when stdin is an open pipe rather than at EOF,
  which makes a run from a pipeline or an editor task runner hang with no
  output and no query running server-side. A script invoked directly needs
  `< /dev/null`. `clickdoom`-based targets (`diff`, `test`,
  `preflight-milestone`, `run-milestone`, `bench-canonical-throughput`) speak
  ClickHouse's HTTP interface directly and need no such workaround.

### Databases

Tests use throwaway databases and never the shared `clickdoom` one.
Each live suite creates its own, named for the suite and the process id. `run-milestone` is the exception and writes to `clickdoom` itself.

## The ClickHouse pin

Pinned to `26.7.5.10` by image digest in `docker-compose.yml`. CI starts the
server through `make up`, so the digest and the server configuration under
`docker/clickhouse/config.d/` are read from that one file everywhere.

Bumping it needs a `ci:` pull request carrying nightly deep-diff evidence. A
minor ClickHouse release can change how an expression is evaluated, and this
project's whole output is an expression evaluated a few billion times.

## Tests

| Target | What it covers |
|---|---|
| `test` | Every suite: the interpreter, riscv-tests inside ClickHouse, the fold, commit and MMIO, frame readout and the committed traces |
| `smoke` | The differential run at 100,000 instructions |
| `gates` | `lint`, the ROM hash check, `test` and `smoke` |

`make test` is two cargo invocations. The first runs the workspace with the
`clickhouse-tests` feature, so the suites needing a server are compiled in and
a missing server is a failure rather than a skipped line. The second runs the
reference-trace and demo3 comparisons in release, which is what makes a
billion-instruction run finish.

A bare `cargo test --workspace` needs no container and no ROM, and visibly
excludes the live suites rather than reporting them as ignored.

### What the smoke run does not cover

`smoke` compares 100,000 instructions at `CHECKPOINT_INTERVAL` spacing, which is
24 register comparisons and **zero memory comparisons**. It never reaches a
`RAM_HASH_INTERVAL` boundary, so `ramhash` and `fbhash` are not checked. A green
smoke run means registers and control flow agreed. It does not mean the
engines agree.

Only a much longer run reaches a memory comparison, which is why one runs
nightly rather than on every pull request. The first boundary alone costs about
14 minutes.

## Native mode

Native mode runs DOOM's simulation and renderer as SQL against a level loaded
from the WAD. Its contract is `NATIVE.md`; its commands live under
`clickdoom native`.

    make native-smoke     # render the first gameplay frame from the probe fixture and check its hash
    make gen-probe-trace  # the reference emulator's per-frame game state for demo3, not committed
    make native-load      # decode the WAD into the native database, plus the probe trace when it exists
    make native-demo      # demo3 at 35 Hz in a window, from the probed states
    make native-play      # the loaded level from the keyboard and mouse
    make native-parity    # every demo3 frame and tic against the reference emulator; exit 3 on the first that differs

The root `README.md` walks the commands from a fresh checkout. `native load` writes only the tables `native/schema.sql` declares and leaves the
rest of the database alone, so it is safe against the shared `clickdoom`
database. `--probe PATH` also loads the reference emulator's state rows, which
is how the renderer is driven before the simulation is complete
(`native demo demo3 --from probe`). `native diff` runs the simulation and
reports the first tic and field on which it and the probe disagree.

The two resident statements stay open for a session and stream one row per
tic; the server settings they need are mounted from
`docker/clickhouse/users.d/` and `docker/clickhouse/config.d/`. A statement
that dies shows up as rows that stop landing, and the session reopens it and
resumes from the last committed tic.

The window needs the driver's `window` feature, on by default; `--no-window`
runs headless and `--frame-dir` writes a PPM per frame (a separate query per
frame, so not a 35 Hz mode). On Linux the window loads X11 or Wayland and the
graphics driver at run time and needs no build-time packages.

The melt's pass count per frame comes from the reference run and is loaded as
data from `driver/melt/demo3.tsv`; its provenance is in that directory's
README.

## Benchmarks

Timings need a quiet machine, and the numbers in `docs/experiments/` were
taken on one.

`make bench-canonical-throughput` is the instrument a throughput claim comes
from. It creates and destroys a container per arm, so it does not touch the
shared one and does not need `make up`. ClickHouse counts executions of an
expression DAG in a process-static map that no `SYSTEM` statement resets, and
the fold-alone and end-to-end arms emit the same step lambda, so two arms
sharing a server share one counter: the first to run pays for the compilation
and the second collects it. `clickdoom emulation bench compare-versions` runs
the same benchmark once per arm image.

Each arm runs `--warmup` batches before it times anything, and the run is
refused unless a warm-up batch compiled something and no timed batch did.
Every batch in the output carries `CompileFunction`,
`CompileExpressionsMicroseconds`, its write-log length, its retired count and
why it stopped.

Two numbers come out of each arm. Instructions per second is the SQL CPU's
own rate. Seconds to first frame divides the ROM's instructions to first
frame, measured by `refemu` in the same run, by that rate, so a ROM change
that retires fewer instructions for the same frame shows up even when the
rate does not move.

Record the ClickHouse version and K with any number you report, and say how
quiet the machine was. A throughput claim without those is not comparable to
anything.

Nothing benchmarks automatically. A shared runner cannot give the timing a
quiet machine gives, and a gate that fails for reasons unrelated to the change
is worse than no gate.

### The machine lock

A timing is only as good as the machine it was taken on, so one holder at a
time announces that the machine is theirs.

    make machine-lock                                 # who holds it
    ./scripts/machine-lock.sh acquire <holder> <why>  # take it
    ./scripts/machine-lock.sh release <holder>        # give it back
    ./scripts/machine-lock.sh break                   # clear a dead holder

The lock is one file, at
`$(git rev-parse --path-format=absolute --git-common-dir)/machine-lock`.
Every worktree of this checkout resolves that to the same path and the same
inode, so a holder in one worktree is visible from all of them. The file
carries the holder's name, when they took it, the host, the process id, the
worktree they took it from, and why.

`acquire` creates the file under `set -C`, which is one atomic open: of two
callers racing, one wins and the other is told who holds it and exits
non-zero. It does not wait. `release` refuses unless the `holder:` line
matches the name given, so only the holder gives it back. A lock left behind
by a run that died is cleared with `break`, which prints what it removed.

`make bench-canonical-throughput` holds the lock for the length of its run and
releases it whether the run succeeds, fails or is interrupted.
`MACHINE_LOCK_HOLDER` is the name it takes the lock under and defaults to
`$USER`; set it to a name whoever reads the lock can reach. Take the lock by
hand for anything else that needs the machine to itself, a long run against
the shared ClickHouse container included: two runners against one database
lose each other's flushes.

The file is not in git, and it is per checkout rather than per machine, so a
second clone of this repository has a lock of its own.

## Reference traces

`refemu/reference_traces/` holds the committed SPEC-format traces, named after
the ROM they came from: `demo-boot-to-first-frame.<rom sha prefix>.tsv`. The
name is derived from `rom/PINNED_HASH`, so a re-pinned ROM cannot silently reuse
the previous one's trace.

`make gen-reference-trace` regenerates one and refuses to run against an
unpinned ROM. The reference interpreter runs at about 170 million instructions
per second on a quiet machine, so a regeneration costs seconds rather than
minutes.

The `demo3` trace is not committed. It is large, changes with every ROM, and is
regenerable.

## The struct layout table

`refemu/probe/layout.tsv` records where the DOOM engine puts each field of each
struct under RV32 ILP32: one `struct field offset size` row per field, plus
each struct's own size under the field name `sizeof`. An array field's size is
its whole extent, so a reader divides to get the element count and needs no
second table of the engine's dimension macros.

The numbers come from the compiler rather than from reading a header.
`rom/toolchain/layout.c` includes the engine headers and emits one `@@` line
per `offsetof`, and `make gen-layout` compiles it with `-S` in the pinned
toolchain container under the ROM's own flags, then writes those lines out. It
is never linked, so it cannot reach the ROM and cannot move `rom/PINNED_HASH`.

`make check-rom-hash` runs `make -C rom check-layout` beside the hash check, so
a ROM change that moves a field fails the gate instead of leaving a reader of
`layout.tsv` pointed at the wrong bytes.

## Labels

`.github/labels.yml` is the taxonomy and `scripts/sync-labels.sh` applies it.
It never deletes, because deleting a label destroys the record of what was
filed under it.

Retiring one is a manual step, and the order matters: move its issues to the
replacement first, then delete it. A label can only be renamed into a name that
does not already exist, so once the replacement exists the rename that would
have preserved the assignments is no longer available.
