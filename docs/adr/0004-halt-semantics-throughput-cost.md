# ADR-0004: Halt/bounds-checking is a real, measured throughput cost

**Status:** accepted (executor, #23).

## Context

ADR-0002's Phase 0 prototype (`fold_predecoded.py`) never implemented SPEC
§1's halt semantics: illegal-opcode detection, `ecall`/`ebreak`/CSR halts,
address-bounds checking, load/store alignment checking, or `SELF_MODIFY`
detection. Its 11,894-13,351 instr/sec numbers (K=50,000, `RESULTS.md`) are
real, but they're the cost of *executing* instructions, not of deciding
whether an instruction is allowed to execute at all.

#23 adds all of the above, required by SPEC §1/§2/§6. Under Phase 0's own
established cost model -- an `arrayFold` step costs ~0.8 µs per expression
node, and neither `multiIf` nor `if` short-circuits an unselected branch --
every one of these checks costs on every instruction, not just the ones that
would actually halt.

## What was measured

`executor/bench/halt_overhead/` reruns Phase 0's own synthetic DOOM-shaped
mix (unchanged: same fractions, same deterministic hash-based fixture) through
`fold.py` instead of `fold_predecoded.py`, same K values, same harness shape.

| K | Phase 0 (fold_predecoded.py) | #23 (fold.py, halt semantics) | delta |
|---:|---:|---:|---:|
| 10,000 | 11,467 / 11,668 | 7,037 / 7,241 | ~-38% |
| 50,000 | 13,178 / 13,351 | 9,085 / 9,090 | ~-31% |
| 200,000 | 13,499 / 13,623 | 9,492 / 9,565 | ~-30% |

A genuine regression, not noise, and over the PR template's 10% bar.

## Where it went, and what was already done about it

An early draft of the accumulator was a flat 11-field tuple (pcidx, regs,
3 write-log arrays, 5 halt-record scalars, retired count). Every field
needs its own `if(step_retires/step_halts_now, ..., previous)` guard, and
under the no-short-circuit cost model that means the halt-detection
condition gets evaluated once per field -- effectively once per accumulator
field, ~10 times a step for a check that logically only needs deciding once.

Two consolidations, both measured:

1. Collapsing four separately-computed booleans (`is_decode_fatal`,
   `mem_bad_addr`, `mem_misaligned`, `self_modify`) plus a duplicate
   halt-reason lookup into one `HALT_CODE` `multiIf` that computes the
   bounds/alignment/self-modify checks exactly once: K=50,000 went from
   ~7,700-8,200 to ~7,700-8,800 instr/sec (modest, within noise).
2. Packing the accumulator from 11 flat fields down to 5 (write-log's three
   arrays into one `tuple`, the halt record's five scalars into another),
   so the halt/retire guard is evaluated per *group* instead of per field:
   K=50,000 went from ~7,700-8,800 to **9,085-9,090** instr/sec -- the
   change that actually moved the number, consistent with Phase 0's finding
   that node count (here: guard-condition duplication count), not data
   volume, is the lever.

Both are structural accumulator-shape choices, not the `arrayMap`-based
let-binding idiom Phase 0 measured as *more* expensive than recomputation
(~4.5 µs/binding) -- no binding trick is used here.

## Decision

Ship at ~9,000-9,600 instr/sec (K=50,000-200,000) rather than block #23 on
closing the remaining ~30% gap. The correctness this buys (SPEC §1's fatal
halts, §2's `BAD_ADDR`, ADR-0002's own `SELF_MODIFY` precondition) is not
optional, and Phase 0's prototype explicitly disclaimed correctness in
exchange for its higher number. ADR-0001's acceptance threshold was ">=
10,000 instr/sec sustained end-to-end" (not fold-in-isolation) -- this PR
does not re-run the full end-to-end harness (batch commit is #25, blocked on
the spec-change in-flight as of this PR), so re-validating against that
specific threshold is deferred to whichever PR first assembles a real
end-to-end loop against a ratified `batch_commit`.

## Consequences

- Filed as a known, accepted cost rather than a bug: the remaining gap is
  structural under `arrayFold`'s cost model, not a leftover inefficiency
  found and left unfixed.
- Follow-up worth filing (not blocking #23): whether any of the four checks
  can be partially precomputed at decode time. Load/store bounds and
  alignment depend on a runtime register value and can't be; but a narrower
  win may exist for `SELF_MODIFY` specifically, since the *text* window is
  static and only the *runtime address* is dynamic -- not explored here.
- `executor/bench/halt_overhead/` stays in the tree as the reproducible
  before/after, the same role `executor/bench/phase0/` plays for ADR-0002.
