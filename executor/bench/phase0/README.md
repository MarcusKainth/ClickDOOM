# Phase 0 arrayFold throughput

The characterisation benchmark behind ADR-0001 and ADR-0002: whether `arrayFold`
can carry a CPU step at all, how the accumulator behaves, and why node count is
the lever that matters.

Findings are in [RESULTS.md](RESULTS.md).

## Run

    make bench-phase0

Emits TSV to stdout as `variant`, `mode`, `K`, `seconds`, `instr_per_sec`, so a
result can be pasted into an ADR or loaded back into ClickHouse.

## What it needs

A quiet machine, and the shared container from `make up`.

## Purity

The harness times query wall-clock on the client side. That is measurement
rather than computation, which PUR-8 allows, and no benchmark number reaches
emulated-CPU state.
