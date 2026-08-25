# Phase 0 benchmark results

Evidence for SPEC §9 and for ADR-0001 / ADR-0002.

Reproduce with `just bench-phase0` (requires `just up`). Raw TSV is what
`run.sh` prints; the tables below are that output, arranged.

## Environment

| | |
|---|---|
| ClickHouse | 26.3.17.4 (the repo pin) |
| Host | Apple Silicon, Docker Desktop, ClickHouse in the pinned container |
| RAM fixture | 24 MiB (6,291,456 words), SPEC §5 shape |
| Text fixture | 2 MiB (524,288 pre-decoded instructions) |
| Instruction mix | synthetic, roughly DOOM-shaped: ~35% ALU, 25% load, 10% store, 18% branch, 5% jump, 7% M-extension |
| `max_threads` | 1 (the fold is inherently sequential) |

Every fixture is generated deterministically from a multiplicative hash of
`number` — no `rand()`, no `now()` (SPEC §8).

## 1. Fold throughput (SPEC §9, first bullet)

The fold in isolation: no state reload, no commit.

| variant | K | seconds | instr/sec |
|---|---:|---:|---:|
| pre-decoded | 10,000 | 0.872 / 0.857 | 11,467 / 11,668 |
| pre-decoded | 50,000 | 3.794 / 3.745 | 13,178 / 13,351 |
| pre-decoded | 200,000 | 14.681 / 14.815 | 13,623 / 13,499 |
| naive (decode in lambda) | 10,000 | 6.333 / 6.480 | 1,579 / 1,543 |

**Pre-decoding is worth 7.4x** — the single largest result of Phase 0, and the
reason ADR-0001 clears its own target instead of missing it by 6x. See ADR-0002.

## 2. Accumulator copy behaviour (SPEC §9, second bullet)

The risk ADR-0001 called out: if a captured constant array were copied into the
accumulator on every step, throughput would collapse as the array grows. Same
fold, same K, only the size of the captured RAM array varies.

| captured RAM array | K | seconds | instr/sec |
|---|---:|---:|---:|
| 1,024 words (4 KiB) | 100,000 | 0.878 | 113,895 |
| 65,536 words (256 KiB) | 100,000 | 0.864 | 115,740 |
| 1,048,576 words (4 MiB) | 100,000 | 0.914 | 109,409 |
| 6,291,456 words (24 MiB) | 100,000 | 0.935 | 106,951 |

**Flat across a 6,144x range in array size.** Captured constant arrays are not
copied per step, and `arrayElement` on them is effectively O(1). Holding all 24
MiB of RAM as a query-level constant is sound. The risk is closed.

(The absolute rate here is high because this is a deliberately trivial fold body
— it isolates array-capture behaviour, not instruction execution.)

## 3. End to end (ADR-0001's actual acceptance criterion)

Sustained: state reload + fold + write-log flush into `ram` + `cpu_state`
update, looped over real batches, wall-clocked.

| K | batches | instructions | seconds | instr/sec |
|---:|---:|---:|---:|---:|
| 10,000 | 60 | 600,000 | 68.797 | **8,721** |
| 50,000 | 12 | 600,000 | 50.444 | **11,894** |
| 200,000 | 3 | 600,000 | 51.598 | **11,628** |

**K = 50,000 is optimal**, which is the value ADR-0001 guessed. At K=10,000 the
~0.30s per-batch fixed cost (0.15s to materialize the 24 MiB RAM constant, 0.017s
for the decode arrays, the rest query analysis) is 30% of the batch; by K=50,000
it is under 8%. Past K=50,000 the write-log's superlinear growth cancels the
remaining amortization.

ADR-0001's threshold was **>=10,000 instr/sec sustained end-to-end**. Met.

## 4. Why node count is the lever

Cost per fold step tracks the number of expression nodes in the lambda, not the
volume of data touched. N chained `bitXor` nodes, K=20,000:

| nodes | seconds | us/node/step |
|---:|---:|---:|
| 2 | 0.071 | 1.77 |
| 20 | 0.270 | 0.68 |
| 100 | 1.260 | 0.63 |
| 400 | 7.881 | 0.99 |

`arrayFold` evaluates its lambda as a full expression pass over a one-row block
per element, so ClickHouse's per-function-call overhead is paid on every node.

Consequences, each measured rather than assumed:

- **Neither `multiIf` nor `if` short-circuits here.** A 40-arm `multiIf` costs
  the same whether arm 0 or arm 39 matches (1.648s vs 1.601s), and
  `if(false, <400 nodes>, cheap)` costs the same as taking the expensive branch.
  Ordering arms by opcode frequency buys nothing.
- **A binary dispatch tree is worse, not better** — 40 leaves at depth 6 measured
  1.861s against the flat `multiIf`'s 1.648s, because the whole tree evaluates.
- **Width is nearly free.** Three register-write strategies (`arrayMap` rebuild
  over 32 elements, bind-then-map, `arraySlice`+`arrayConcat`) landed within 2%
  of each other, because a 32-row block costs about what a 1-row block costs.
- **`short_circuit_function_evaluation` does not rescue it.** `enable`,
  `force_enable` and `disable` measured 6.385s / 6.451s / 6.4s on the realistic
  fold — indistinguishable.
- **The let-binding idiom `arrayMap(v -> ..., [expr])[1]` costs ~4.5us per
  binding.** Recomputing a cheap subexpression twice beats binding it once.

## 5. Where the remaining time goes

Ablation at K=50,000, pre-decoded variant (3.629s baseline):

| removed | seconds | share of total |
|---|---:|---:|
| M-extension (8 arms) | 2.962 | 18% |
| branch arms (6) | 3.222 | 11% |
| store path | 3.248 | 10% |
| load path | 3.308 | 9% |
| write-log probe on loads | 3.453 | 5% |
| everything else (ALU arms, field reads, register write, fold overhead) | — | ~52% |

No hotspot. Cost is diffuse, exactly as the node-count model predicts. The
M-extension is the largest single group and — because nothing short-circuits —
it costs that 18% on *every* instruction, not just on multiplies. Collapsing
those eight arms using pre-decoded signedness flags is the most valuable
optimization left, and it is filed against the `sqlcpu` and `executor`
workstreams.

## 6. What this does not measure

Stated plainly so nobody mistakes this for more than it is:

- **Correctness.** The "ROM" is pseudo-random words and the decode table is a
  synthetic mix. riscv-tests is what proves the CPU right (`sqlcpu` workstream).
- **The real instruction mix.** Since `multiIf` does not short-circuit, the mix
  barely affects throughput — but it does decide which arms are worth collapsing
  next. refemu's boot histogram will tell us.
- **MMIO, frame commit, and the render query.** None are in the fold yet.
- **CI hardware.** These numbers are from one developer machine. The nightly
  bench establishes the real baseline and gates regressions against it.
