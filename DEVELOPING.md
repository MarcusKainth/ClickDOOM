# Developing ClickDOOM

Maintainer and returning-contributor reference: the build, test and benchmark
mechanics. [`CONTRIBUTING.md`](CONTRIBUTING.md) is the entry point and covers
policy; this file covers how things run.

## Prerequisites

| Tool | Why |
|---|---|
| Docker | ClickHouse and the rv32 toolchain both run in containers |
| `make` | Every task. `make help` lists them |
| `uv` | Python environments for `refemu` and `executor`, both with committed lockfiles |
| `shellcheck` | `make shellcheck` |
| `clang-format` | `make clang-format`, over `rom/src/` |
| `actionlint`, `zizmor` | Workflow linting, both in `make lint` |

Python is 3.11 or newer. The rv32 toolchain is pinned to xPack
`riscv-none-elf-gcc` 15.2.0-1 and built from `rom/toolchain/Dockerfile`; you
never install it yourself.

## Targets

`make help` is the inventory. What it does not say:

- **`make up` is a prerequisite of most test and bench targets**, so you rarely
  run it yourself. It waits for the container to report healthy.
- **`test-sqlcpu`, `test-executor`, `test-render`, `diff` and most benches need
  a live ClickHouse.** `test-refemu`, `lint` and `build-rom` do not.
- **Targets are not parallel-safe.** Most share one container, and the
  compiled-expression cache is server-global, so two timing runs at once
  measure each other. Do not pass `-j`.
- **Every ClickHouse-driving target closes stdin.** `clickhouse-client` blocks
  forever on an INSERT when stdin is an open pipe rather than at EOF, which
  makes a run from a pipeline or an editor task runner hang with no output and
  no query running server-side. A script invoked directly needs `< /dev/null`.

### `CH_CLIENT`

`driver/test_render.sh` expects `clickhouse-client` on `PATH` and does not
auto-detect one the way `scripts/diff_run.sh` does. On a host without it:

    make test-render CH_CLIENT="docker exec -i clickdoom-ch clickhouse-client"

### Databases

Tests use throwaway databases and never the shared `clickdoom` one.
`test-render` creates `driver_render_test_<pid>`, the benches use
`clickdoom_exec_bench` and similar. `run-milestone` is the exception and writes
to `clickdoom` itself.

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
| `test-refemu` | riscv-tests and the trace emitter, against the reference interpreter |
| `test-sqlcpu` | riscv-tests inside ClickHouse, decode vectors, execute and checkpoint checks |
| `test-executor` | Fold, commit and MMIO, against a fixture schema |
| `test-render` | Frame readout and the ANSI and PPM render queries |
| `smoke` | The differential run at 100,000 instructions |

`make test` runs the suites above. It does not run `smoke`, which needs a
built ROM.

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

Timings need a quiet machine, and the numbers in the committed `RESULTS.md`
files were taken on one.

Several benches create and destroy their own container per arm, because the
compiled-expression cache is server-global and would otherwise carry state
between arms. Those do not depend on `make up`.

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
unpinned ROM. The reference interpreter runs at roughly a million instructions
per second, which sets the cost of any regeneration.

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
