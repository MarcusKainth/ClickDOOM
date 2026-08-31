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

Pinned to `26.7.5.10` by image digest. The digest appears in
`docker-compose.yml` and once per service block in the workflows, because GitHub does
not expose the `env` context to `services.image` and the literal cannot be
shared. All five must match.

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

## Benchmarks

Timings need a quiet machine, and the numbers in `docs/experiments/` were
taken on one.

`make bench-canonical-throughput` is the instrument a throughput claim comes
from. `clickdoom bench compare-versions` creates and destroys its own
container per arm, because the compiled-expression cache is server-global and
would otherwise carry state between arms.

Record the ClickHouse version and K with any number you report, and say how
quiet the machine was. A throughput claim without those is not comparable to
anything.

Nothing benchmarks automatically. A shared runner cannot give the timing a
quiet machine gives, and a gate that fails for reasons unrelated to the change
is worse than no gate.

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

## Labels

`.github/labels.yml` is the taxonomy and `scripts/sync-labels.sh` applies it.
It never deletes, because deleting a label destroys the record of what was
filed under it.

Retiring one is a manual step, and the order matters: move its issues to the
replacement first, then delete it. A label can only be renamed into a name that
does not already exist, so once the replacement exists the rename that would
have preserved the assignments is no longer available.
