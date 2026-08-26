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

| K | Phase 0 (fold_predecoded.py) | #23, first cut | #23, + x0/link-value fix | delta vs Phase 0 |
|---:|---:|---:|---:|---:|
| 10,000 | 11,467 / 11,668 | 7,037 / 7,241 | 5,889 / 5,959 | ~-49% |
| 50,000 | 13,178 / 13,351 | 9,085 / 9,090 | 7,406 / 7,532 | ~-44% |
| 200,000 | 13,499 / 13,623 | 9,492 / 9,565 | 7,637 / 8,138 | ~-42% |

A genuine regression, not noise, and over the PR template's 10% bar. The
"+x0/link-value fix" column is a second, later cost: sqlcpu's real register
file (`schema.sql`, PR #42) has no array slot for x0 -- 31 elements, x1..x31
-- where the Phase 0 bench this PR started from used a 32-element array with
x0 pinned at a slot. Every read of `rs1`/`rs2` now needs an explicit
`if(r=0, 0, regs[r])` guard instead of a bare array access, and `A`/`B` (that
guarded read) are each substituted many times across `RESULT`/`NEXT`/`ADDR` --
the same multiplicative cost pattern as the halt-check consolidation below,
just on a value that's harder to restructure away (unlike the halt/write-log
fields, `A`/`B` feed directly into many independently-shaped expressions,
not parallel accumulator slots that can be packed into one guarded tuple).
Not attempted further in this PR; see Consequences.

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

Ship at ~7,400-8,100 instr/sec (K=50,000-200,000) rather than block #23 on
closing the remaining ~42% gap. The correctness this buys (SPEC §1's fatal
halts, §2's `BAD_ADDR`, ADR-0002's own `SELF_MODIFY` precondition, and
sqlcpu's actual 31-element register-file convention) is not optional, and
Phase 0's prototype explicitly disclaimed correctness in exchange for its
higher number -- two of the corrections here (the register file, and the
jal/jalr link-value/jump-target split) are defects `sqlcpu` found reviewing
PR #42's schema against this PR's fold, not choices. ADR-0001's acceptance
threshold was ">= 10,000 instr/sec sustained end-to-end" (not
fold-in-isolation) -- this PR does not re-run the full end-to-end harness
(batch commit is #25, blocked on the spec-change in-flight as of this PR),
so re-validating against that specific threshold is deferred to whichever
PR first assembles a real end-to-end loop against a ratified `batch_commit`.
That threshold is now genuinely at risk given how much of the fold-in-
isolation number has gone to correctness since Phase 0 -- worth the team
lead's attention if #25's end-to-end number comes in under 10,000.

## Consequences

- Filed as a known, accepted cost rather than a bug: the remaining gap is
  structural under `arrayFold`'s cost model, not a leftover inefficiency
  found and left unfixed.
- Follow-up worth filing (not blocking #23): whether any of the four halt
  checks can be partially precomputed at decode time. Load/store bounds and
  alignment depend on a runtime register value and can't be; but a narrower
  win may exist for `SELF_MODIFY` specifically, since the *text* window is
  static and only the *runtime address* is dynamic -- not explored here.
- Also worth a follow-up: whether `A`/`B`'s x0 guard can be restructured to
  cost less, e.g. by computing them once into a scalar-only sub-tuple the
  way the halt record was packed -- not attempted here because, unlike the
  halt record, `A`/`B` aren't parallel accumulator slots; they're inputs to
  many independently-shaped downstream expressions, so the same packing
  trick doesn't obviously apply. Someone should look harder at this before
  accepting ~42% as the floor.
- `executor/bench/halt_overhead/` stays in the tree as the reproducible
  before/after, the same role `executor/bench/phase0/` plays for ADR-0002.
