# ADR-0002: Pre-decoded instruction table, and immutable text

**Status:** accepted (Phase 0)

## Context

ADR-0001 chose `arrayFold` for batch execution and listed the risks worth
measuring. The Phase 0 benchmark found a cost model it did not anticipate, and
that model — not any of the listed risks — turned out to dominate.

**An `arrayFold` step costs roughly 0.8 us per expression node in the lambda,
almost independently of how much data those nodes touch.** Measured by chaining
N trivial `bitXor` nodes in a fold body over 20,000 elements:

| nodes in lambda | seconds | us per node per step |
|---|---|---|
| 2   | 0.071 | 1.77 |
| 20  | 0.270 | 0.68 |
| 100 | 1.260 | 0.63 |
| 400 | 7.881 | 0.99 |

That chain carries new literals alongside its nodes, so the per-node column
prices both together. `docs/experiments/compiled-node-cost.md` separates them
on 26.7.5.10: a node is 4.4 ns compiled and 0.29 us interpreted, and each
distinct literal costs 0.16 to 0.28 us per step. Read "the only lever on
throughput is the total node count" below as the reasoning this decision was
taken under. The decision itself rests on a measured before and after on the
same fold, 7.4x, which does not depend on the model.

The reason is that `arrayFold` evaluates its lambda as a full expression pass
over a one-row block per element, and ClickHouse's per-function-call overhead is
paid on every node regardless of block width. Two consequences follow, and both
were verified rather than assumed:

- **Width is nearly free.** An `arrayMap` over a 32-element register array
  inside the lambda costs about the same as evaluating the same expression once,
  because it is one pass over a 32-row block. Three different register-write
  strategies (rebuild via `arrayMap`, bind-then-map, `arraySlice`+`arrayConcat`)
  measured within 2% of each other.
- **Branches are not free.** Neither `multiIf` nor nested `if` short-circuits
  here. A 40-arm `multiIf` costs the same whether arm 0 or arm 39 matches
  (1.648s vs 1.601s over 20,000 steps), and `if(false, <expensive>, cheap)`
  costs the same as evaluating the expensive branch. A binary dispatch tree over
  40 leaves measured *worse* than the flat `multiIf` (1.861s vs 1.648s).
  Ordering arms by opcode frequency buys nothing. This is a cost-equivalence
  result, measured by timing, and it does not carry to faults. What raises
  from an unselected arm on 26.7.5.10 is narrower, and it is the standing
  constraint on every fold expression written after this ADR:
  - **A guarded `intDiv` or `modulo` whose divisor is a constant raises.** The
    guard, `if` or `multiIf`, does not protect it.
    `FunctionBinaryArithmetic.h`'s `isSuitableForShortCircuitArgumentsExecution`
    returns false when the divisor argument is constant, so the division is not
    lazy and runs on every row. `intDiv(INT_MIN, -1)` behind a guard that is
    always false for real data is exactly this shape (#99). It was caught by
    code review the same day it was filed and never observed in a real run.
  - **A guarded `intDiv` or `modulo` whose divisor is computed from data does
    not raise**, because the same predicate makes it a short-circuit argument
    and the guard skips it. It does raise at
    `short_circuit_function_evaluation = 'disable'`, which the executor does
    not set.
  - **A guarded array index does not raise.** An `arrayElement` index computed
    from data returns the element type's default when it is zero or out of
    range. A literal index of `0` raises whether it is guarded or not, so no
    guard was ever standing between it and the fault.

So the only lever on throughput is the total node count of the fold body. The
obvious implementation — fetch the word, pull fields apart with bit ops,
dispatch on opcode/funct3/funct7 — generates roughly 700 nodes and runs at
**~1,580 instructions/sec**, six times below ADR-0001's 10k target.

## Decision

**Decode is a table, not an expression.** The executor builds a pre-decoded
instruction table inside ClickHouse, captures its columns as constant arrays,
and the fold body reads fields with `arrayElement` instead of reconstructing
them with bit arithmetic. PURITY.md already permits exactly this: "Decoding the
ROM *inside* ClickHouse into a decoded-instruction table is fine — that's SQL
doing the work."

The opcode space is collapsed so the execute `multiIf` has as few arms as
possible. Two collapses carry most of the benefit:

- **I-type and R-type become one arm.** The decoder writes `rs2 = 0` for I-type
  (x0 is hardwired zero) and `imm = 0` for R-type, so `b = regs[rs2] + imm`
  produces the correct second operand for both with no branch. `addi` and `add`
  are the same arm; nine I-type arms disappear.
- **`lui`, `auipc` and the `jal`/`jalr` link value become one arm.** All are
  "put a constant in rd", and every one of those constants is static per pc, so
  the decoder precomputes it and they reuse the `add` arm with `rs1 = rs2 = 0`.

Loads (`lb`/`lh`/`lw`/`lbu`/`lhu`) collapse to a single arm driven by a
pre-decoded width mask and sign bit; stores (`sb`/`sh`/`sw`) collapse to a
single read-modify-write arm the same way. Branch targets are pre-decoded to
absolute word indices, so no immediate reconstruction survives in the lambda.

**This requires the text segment to be immutable.** A pre-decoded table is only
sound if the instruction words it was built from cannot change underneath it.
The ROM's linker script places code in its own region, and a store into that
region is a fatal halt with reason `SELF_MODIFY` (SPEC §1). DOOM does not
self-modify; if it ever appears to, that is a bug worth halting on rather than a
feature worth supporting.

## Consequences

- Throughput went from ~1,580 to ~13,400 instructions/sec on the same fold, a
  **7.4x speedup**, which is what moves ADR-0001 from failing its own target to
  clearing it.
- `SELF_MODIFY` is a new cross-workstream contract: `rom` must not write to
  text, `refemu` must halt identically, `sqlcpu` must enforce it. It is
  therefore in SPEC, not only here.
- The decode table is built from RAM by a SQL query at ROM load, and covers only
  the text segment (~2 MiB), so capturing its columns costs ~0.017s per batch
  against ~0.15s for the 24 MiB RAM array.
- Cost after the change is diffuse rather than hot — no single arm dominates.
  Ablations at K=50,000: M-extension arms 18%, load path 9%, store path 10%,
  branch arms 11%, everything else 52%. Further gains have to come from more
  collapsing (signedness-driven `mul`/`div` arms are the obvious next target),
  not from finding a hotspot.
- The naive decode-in-lambda generator was the baseline measured in
  `docs/experiments/arrayfold-baseline.md` as
  the control, so this claim stays reproducible rather than becoming folklore.
