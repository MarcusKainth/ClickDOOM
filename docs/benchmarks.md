# Benchmarks

Every harness in the tree, what it settles, and where its findings are. A
finding nobody can find is not evidence.

Each directory holds a `README.md` describing the harness and how to run it. A
`RESULTS.md` beside it holds the findings, dated and with the ClickHouse version
they were taken against. A directory with no `RESULTS.md` has had no findings
recorded for it; the table below says which.

Timings need a quiet machine. `DEVELOPING.md` says what that means and what to
record alongside a number.

## Throughput and the batch loop

| Harness | Question it settles | Findings |
|---|---|---|
| `executor/bench/phase0/` | Can `arrayFold` carry a CPU step, and what is the lever? Evidence for ADR-0001 and ADR-0002 | `RESULTS.md` |
| `rom/bench/canonical_throughput/` | Real-ROM throughput on boot and gameplay windows, fold-alone and end to end | none recorded |
| `executor/bench/batch_overhead/` | Splits end-to-end overhead into state reload and write-log flush | none recorded |
| `executor/bench/halt_overhead/` | What the fold's halt semantics cost. The before-and-after for ADR-0004 | none recorded |
| `executor/bench/commit_mutation/` | Per-statement attribution of one end-to-end batch | `RESULTS.md` |
| `executor/bench/wl_seed/` | Does per-instruction cost grow within a batch? | `RESULTS.md` |
| `executor/bench/hwm/` | Where the write-log flush stops being cheap. Sets the high-water mark default | `RESULTS.md` |

## Expression evaluation

| Harness | Question it settles | Findings |
|---|---|---|
| `executor/bench/a1_jit/` | What ClickHouse's expression JIT compiles in the fold step, and what that buys | `RESULTS.md` |
| `executor/bench/e1_cse/` | Does `arrayFold` deduplicate repeated subexpressions, and at what node cost | `RESULTS.md` |
| `executor/bench/b2_block_dispatch/` | What an unselected branch costs inside `arrayFold` | `RESULTS.md` |
| `executor/bench/b3_dict_lookup/` | `dictGet` against `arrayElement` for RAM reads | `RESULTS.md` |

## Environment and the ROM

| Harness | Question it settles | Findings |
|---|---|---|
| `executor/bench/b1_native/` | Native ClickHouse against Docker Desktop | `RESULTS.md` |
| `rom/bench/e7_memfns/` | Are `memcpy` and `memset` byte-loop shims, and what do they cost? | `RESULTS.md` |
