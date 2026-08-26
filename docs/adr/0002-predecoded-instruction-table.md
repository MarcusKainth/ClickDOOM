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
  Ordering arms by opcode frequency buys nothing. This is a *cost*-equivalence
  result (measured via timing); issue #183 establishes the stronger
  *fault*-equivalence claim the timing result only suggests — every arm of
  every `if`/`multiIf` inside an `arrayFold` executes for its faults too,
  unconditionally, regardless of its guard (a data-dependent, non-foldable
  guard; a literal `false` guard is constant-folded away and proves nothing).
  A standing constraint on every fold expression written after this ADR, not
  just this decision's own justification — see that issue before adding a
  guarded **division, modulo, or array index** inside a fold: each is an
  *unconditional* fault, not a conditional one, once it's inside the lambda.
  `intDiv(INT_MIN, -1)` behind a guard that's always false for real data is
  exactly this shape (#99) — it would have stalled a multi-day run
  permanently, had one been attempted; #99 was actually caught by code
  review the same day it was filed (10:59→12:18Z), never observed in a
  real run.

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
- The naive decode-in-lambda generator is kept in `executor/bench/phase0/` as
  the control, so this claim stays reproducible rather than becoming folklore.
