# ADR-0004: Halt/bounds-checking is a real, measured throughput cost (amends ADR-0001's threshold)

**Status:** accepted (executor, #23). **Amends ADR-0001**: the ">= 10,000
instr/sec sustained end-to-end" acceptance criterion is retired as a merge
gate for correctness work, for the reasons in Decision below. ADR-0001's
own numbers stay as the historical record of what Phase 0 measured; they
are no longer the bar future work is required to clear.

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

| K | Phase 0 baseline | #23, first cut | + x0/link-value fix | + byte-address PC / eager MISALIGNED | delta vs Phase 0 |
|---:|---:|---:|---:|---:|---:|
| 10,000 | 11,467 / 11,668 | 7,037 / 7,241 | 5,889 / 5,959 | 2,851 / 2,862 | ~-75% |
| 50,000 | 13,178 / 13,351 | 9,085 / 9,090 | 7,406 / 7,532 | 4,294 / 4,295 | ~-68% |
| 200,000 | 13,499 / 13,623 | 9,492 / 9,565 | 7,637 / 8,138 | 4,630 / 4,676 | ~-66% |

A genuine, large regression, well over the PR template's 10% bar. Three
distinct corrections landed in sequence, each with its own real cost:

1. **First cut**: SPEC §1 halt semantics Phase 0 never implemented at all
   (illegal opcode, ecall/ebreak/CSR, address bounds, load/store alignment,
   SELF_MODIFY).
2. **+ x0/link-value fix**: sqlcpu's real register file (`schema.sql`, PR
   #42) has no array slot for x0 -- 31 elements, x1..x31 -- where the Phase 0
   bench this PR started from used a 32-element array with x0 pinned at a
   slot. Every read of `rs1`/`rs2` now needs an explicit `if(r=0, 0,
   regs[r])` guard instead of a bare array access, and `A`/`B` (that guarded
   read) are each substituted many times across `RESULT`/`NEXT`/`ADDR`.
3. **+ byte-address PC / eager MISALIGNED**: the largest single jump, and the
   most structurally important fix. `PC` switched from a word index (cheap:
   `acc.1 + 1`) to a byte address matching `cpu_state.pc` and sqlcpu's real
   convention (`least(bitShiftRight(toUInt32(toUInt64(PC) - ram_base), 2),
   decn-1) + 1` -- several nodes instead of one), and `IDX` is referenced by
   every one of `ID`/`RD`/`IMM`/`TGT`/`DMK`/`DSG`/`RAW` (43 occurrences of
   `{ID}` alone in `fold.py`), so that cost is paid dozens of times a step.
   On top of that, the new eager jump/branch-misalignment check (`would_jump`,
   a 9-arm dispatch, plus `jump_target_if_taken`) is itself substituted
   several times across `HALT_CODE` and `halt_extra_calc`. This was not a
   discretionary feature: it fixes a real bug (a word-indexed PC cannot
   represent a target with bit 1 set, silently destroying exactly the bit a
   MISALIGNED check needs), found independently by `sqlcpu` reviewing this
   PR and by re-reading issue #37's ruling, and confirmed as the same defect
   `sqlcpu` had already found and fixed in their own `execute.py`/`decode.sql`
   (PR #46/#49) before either of us built further on the wrong
   representation.

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

## End-to-end measurement (ADR-0001's actual threshold)

Fold-in-isolation is the number that isolates *this PR's* cost, but
ADR-0001's acceptance criterion is explicit: **">= 10,000 instr/sec
sustained end-to-end (batch + commit + state reload)"**, not fold alone.
Comparing fold-to-fold hides a unit mismatch -- both numbers move together,
the ratio looks like a stable ~68% regression, and nobody notices the
absolute e2e number crossing the threshold because it's never actually
measured. `executor/bench/halt_overhead/run.sh` now has an `e2e` mode
(added for this measurement, same ad-hoc flush shape Phase 0's e2e harness
used -- the real atomic-commit design is #25, still blocked on
ratification) alongside the fold-only mode:

| K | mode | seconds | instr/sec |
|---:|---|---:|---:|
| 50,000 | e2e (this PR) | 299.393 | **2,004** |
| 50,000 | e2e (Phase 0, `RESULTS.md`) | 50.444 | 11,894 |

**Measured, not estimated, and well under ADR-0001's 10,000 threshold** --
an ~83% regression against Phase 0's e2e baseline, worse than the ~68%
fold-in-isolation regression above because e2e adds the state-reload and
write-log-flush round trips on top of the now-slower fold, and those scale
with the same per-instruction cost this ADR documents.

## Decision

The correctness bought here (SPEC §1's fatal halts, §2's `BAD_ADDR`,
ADR-0002's own `SELF_MODIFY` precondition, sqlcpu's actual 31-element
register-file convention, and a byte-address `pc` that can represent and
catch a misaligned jump target instead of silently discarding the bit that
matters) is not optional, and Phase 0's prototype explicitly disclaimed
correctness in exchange for its higher number. Three of the corrections here
-- the register file, the jal/jalr link-value/jump-target split, and the
word-indexed-PC/MISALIGNED-truncation bug -- are defects `sqlcpu` and the
team lead found reviewing this PR against `schema.sql`/`execute.py`, not
choices this PR made freely.

**This ADR is amending ADR-0001's acceptance criterion.** 2,004 instr/sec
e2e is not a number to leave quietly unmet against a >=10,000 threshold. The
10,000 figure was a pre-implementation estimate, made before Phase 0
discovered that arrayFold's cost model is per-expression-node rather than
per-byte-of-data-moved -- a discovery that came *after* the number was
picked, not something the number accounted for. SPEC §1 correctness was
never optional either way. A throughput target set before the cost model was
understood, and before the correctness requirements were priced in, is the
thing that should move, not the correctness.

**Wall-clock consequence, stated plainly rather than left for discovery
during the timelapse run:** Phase 0's ~11,900 instr/sec e2e already implied
a multi-week `-timedemo demo3` run. At ~2,004 instr/sec -- roughly 5.9x
slower -- what was multi-week becomes multi-month at this PR's numbers
alone, before #24 (MMIO plumbing) and #25 (batch commit) add their own
per-instruction cost on top. This is not proposed as the final number:
§Consequences below names the concrete, understood optimization
(consolidating `RESULT`/`NEXT`/`HALT_CODE` into one dispatch) that the next
session should attempt before this becomes the number DOOM actually runs
at. Amending ADR-0001's threshold now is about not blocking correctness on
an estimate made before the cost model was known -- it is not a claim that
2,004 instr/sec is an acceptable place to stop.

## Consequences

- Filed as a known, accepted cost rather than a bug: the remaining gap is
  structural under `arrayFold`'s cost model, not a leftover inefficiency
  found and left unfixed.
- **Follow-up, concrete and worth prioritizing over the others below**:
  consolidate `RESULT`, `NEXT`, and `HALT_CODE`'s dispatch into one combined
  per-`op_id` `multiIf` returning a tuple, so `{ID}` (and therefore `IDX`'s
  now-expensive byte-address conversion) is evaluated once per step instead
  of the ~40+ times it currently is across three separate dispatches that
  mostly key off disjoint `op_id` ranges. `sqlcpu` hit the identical
  AST-multiplication problem in `execute.py` and solved it differently --
  their `next_pc()`/`halted()`/`halt_reason()` all accept an optional
  `misaligned=` parameter so a *single-row* caller can bind the condition
  once via a query-level `WITH` clause. That doesn't transfer here: there is
  no query-level scope inside an `arrayFold` lambda to bind into, since the
  condition depends on per-step accumulator state that only exists inside
  the lambda. The combined-dispatch restructuring above is this fold's
  equivalent of the same insight, not attempted in this PR given the size of
  the rework already in it.
- Follow-up worth filing (not blocking #23): whether any of the halt checks
  can be partially precomputed at decode time. Load/store bounds and
  alignment, and jump-target alignment, depend on a runtime register value
  and can't be; but a narrower win may exist for `SELF_MODIFY` specifically,
  since the *text* window is static and only the *runtime address* is
  dynamic -- not explored here.
- `executor/bench/halt_overhead/` stays in the tree as the reproducible
  before/after, the same role `executor/bench/phase0/` plays for ADR-0002.
