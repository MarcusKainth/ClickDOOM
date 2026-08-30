# Halt overhead

What the fold's halt semantics cost. Compares `executor/fold.py`, which carries
halt checks, write-log versioning, and address, alignment and self-modify
checks, against the Phase 0 baseline in `executor/bench/phase0/`, over the same
synthetic instruction stream.

ADR-0004 names this directory as the reproducible before-and-after for that
decision, the same role `phase0/` plays for ADR-0002.

## Run

    make bench-halt-overhead

Sweeps K. Creates and drops its own private database rather than using the
shared one, so a concurrent run cannot corrupt it mid-sweep.

Override `CLICKDOOM_BENCH_KS`, `CLICKDOOM_BENCH_REPEATS`, `CLICKDOOM_BENCH_HWM`
and `CLICKDOOM_BENCH_DB`.

## What it needs

A quiet machine.
