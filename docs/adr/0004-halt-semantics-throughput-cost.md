# ADR-0004: Halt/bounds-checking is a real, measured throughput cost (amends ADR-0001's threshold)

**Status:** accepted (executor, #23). **Amends ADR-0001**: the ">= 10,000
instr/sec sustained end-to-end" acceptance criterion is retired as a merge
gate for correctness work, for the reasons in Decision below. ADR-0001's
own numbers stay as the historical record of what Phase 0 measured; they
are no longer the bar future work is required to clear.

**The current, final, verified numbers** (K=50,000; superseding every
intermediate figure elsewhere in this document, which is kept as the
measurement history, not as competing conclusions): **fold-in-isolation
~1,860 instr/sec, end-to-end 1,159 instr/sec**, both confirmed via
`batch_out.retired`/`select_only`'s own `retired` field showing full,
non-halting execution (50,000/50,000 per fold call, 600,000/600,000 across
the 12-batch e2e run) -- not inferred from wall-clock alone. **1,159 is
159 instr/sec above the human owner's standing 1,000 instr/sec e2e
escalation floor** (team lead, no fallback reaching it triggers a report) --
a real margin, but a thin one, given #24 and #25 both still add cost to the
same paths. `demo3` is 2,134 tics (read from the WAD directly, not
estimated) at refemu's measured ~1.36M instructions/tic -- 2.91 billion
instructions. At 1,159 instr/sec that run is **~29 days**.

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

**Caveat on the table immediately below**: these fold-in-isolation numbers
were measured before the "End-to-end measurement" section's harness bugs
were found and fixed (the mix halting after 1 instruction, addressed there
in detail). Per Phase 0's own finding, `arrayFold`'s per-step cost doesn't
depend on whether a step retires, so the *relative* progression across the
three fixes below should still hold -- but the corrected, non-halting mix
measured real run-to-run variance (2,437 vs 3,867 instr/sec, same K, back to
back) that this table's numbers, frozen at a near-empty write-log
throughout, don't show. Treat this table as directionally right and the
End-to-end section's numbers as the ones actually re-verified after the fix.

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
| 50,000 | e2e (this PR, corrected -- see below) | 313.077 | **1,916** |
| 50,000 | e2e (this PR, first measurement, invalid) | 299.393 | 2,004 |
| 50,000 | e2e (Phase 0, `RESULTS.md`) | 50.444 | 11,894 |

**The first e2e measurement (2,004) was invalid, not just imprecise --
found by checking `batch_out.retired` after the fact, not by trusting the
wall-clock number.** Two real bugs in this PR's own benchmark harness (not
in `fold.py`/#23's shipped design), both in `executor/bench/halt_overhead/`:

1. **The synthetic mix halted on its first load/store, then stayed halted.**
   Phase 0's mix used raw small `imm` values as load/store addresses,
   correct only because Phase 0's fold had no bounds checking. #23 checks
   `regs[rs1] + imm` against `[RAM_BASE, RAM_BASE + RAM_BYTES)`, and with
   `regs[rs1]` starting at 0, an unadjusted `imm` in `[0, 4096)` is never
   inside RAM -- `BAD_ADDR` on the very first load/store, every "batch"
   after that re-hitting the same frozen halt instantly. `retired` was 1,
   not 50,000, on every one of the 12 batches the harness ran. Fixed:
   load/store (and separately, `jalr`, whose register-relative target
   turned out to hit `MISALIGNED` about half the time given an otherwise-
   unconstrained accumulated register value) now force `rs1 = 0` and use a
   deterministically in-bounds, aligned, non-text address.
2. **The `ram` flush query cross-joined instead of zipping.** Three
   independent `arrayJoin(...)` calls on different expressions
   (`arrayZip(wl_addr, wl_val)` twice, `wl_icount` separately) produce the
   *Cartesian product* of the joined arrays in ClickHouse, not a parallel
   walk, unless they're the textually identical expression. With ~6,400
   stores in a full batch that's ~41 million wrong rows instead of 6,400
   right ones. Fixed by zipping all three arrays together in one
   `arrayZip(wl_addr, wl_val, wl_icount)` call, referenced identically three
   times (`.1`/`.2`/`.3`) so ClickHouse recognizes it as one join.

Both are fixed in `executor/bench/halt_overhead/{setup.sql,run.sh}`.
**The corrected number, 1,916 instr/sec, is close to the invalid one** --
confirming, rather than undermining, Phase 0's own finding that `arrayFold`
evaluates every step's full expression cost regardless of whether the step
actually retires. A batch that halts after 1 real instruction and one that
completes all 50,000 pay nearly the same fold cost; the visible difference
in this data is the flush/commit path scaling with write-log size (fixed
bug #2 above), which turned out to be a comparatively small fraction of the
total either way. **Still well under ADR-0001's 10,000 threshold** -- an
~84% regression against Phase 0's e2e baseline, worse than the ~68%
fold-in-isolation regression above because e2e adds the state-reload and
write-log-flush round trips on top of the now-slower fold.

### A third, unrelated cost landed on top of this: the groupArray capture fix

Separately, sqlcpu found (PR #67) that `DECODE_WITH`'s per-column
`groupArray(col)` idiom is not reliably safe against `word_addr` in
ClickHouse 26.3 -- `optimize_read_in_order` can stream a column straight
from physically-sorted storage, bypassing the subquery's `ORDER BY`, and
silently misalign one column while its siblings stay correct. Could not
reproduce it against this PR's own tables despite real effort (documented
in the PR thread), but the fix -- one combined `groupArray(tuple(...))` per
table instead of one per column -- is free of any correctness downside and
removes a setting-dependent landmine, so it's applied regardless of whether
it's currently biting this specific table's size/shape. Applied in the same
pass as this ADR's other numbers.

It is not free of throughput cost: fold-in-isolation at K=50,000 on the
corrected (non-halting) mix went from a noisy 2,437-3,867 instr/sec to a
stable 1,757-1,898 -- a further real regression, though also notably *more
consistent* run to run, which may mean the earlier noise was itself an
artifact of the vulnerable capture pattern rather than genuine variance in
write-log-length-dependent cost.

### Final clean re-derivation (both bugs fixed, both benchmarks re-run together)

The team lead's suspicion, stated plainly before this ran: if the arrayJoin
cross-join was the real source of the "batch overhead is 53% of e2e, 33x
worse than Phase 0" conclusion, fixing it might make that whole
investigation dissolve -- there would be nothing left to profile. Tested by
re-running fold-in-isolation and e2e together, same fixture, both bugs
fixed (the halting mix, the arrayJoin cross-join, and the groupArray capture
fix, all three landed by the time this ran):

| K | mode | seconds | instr/sec |
|---:|---|---:|---:|
| 50,000 | fold | 26.176 | 1,910 |
| 50,000 | fold | 27.575 | 1,813 |
| 50,000 | e2e (600,000 instructions, 12 batches) | 517.538 | 1,159 |

|  | Phase 0 | now (clean) |
|---|---:|---:|
| fold | 76 µs/instr | 537.5 µs/instr |
| e2e | 84 µs/instr | 862.6 µs/instr |
| batch overhead | 8 µs/instr (9.5% of e2e) | 325.1 µs/instr (37.7% of e2e) |
| e2e/fold ratio | 0.90 | 0.62 |

**It did not dissolve, but it did shrink a lot.** The arrayJoin bug was
real and worth fixing, but it was not the entire story: batch-commit
overhead is still ~41x worse than Phase 0's baseline and still over a
third of end-to-end time, down from the earlier (invalid) 53%/33x estimate
but not down to noise. There is a real, smaller lever left in the
state-reload/flush path -- the investigation the team lead originally
asked for is still warranted, just smaller in scope than first estimated.
Not profiled further in this PR; handed off as the concrete next step with
a verified baseline to measure against, rather than the guessed-at one this
ADR started with.

One more thing this correction surfaced, not yet explained: fold-in-isolation
itself was noisier across repeats with the corrected (non-halting) mix than
before the groupArray fix landed -- 2,437 and 3,867 instr/sec on the same K,
same fixture, back to back, versus the earlier halted-mix runs which agreed
within a few percent, and versus the post-groupArray-fix runs (1,910/1,813
above), which are close together again. A write-log that actually grows to
thousands of entries makes each load's `arrayLastIndex` scan genuinely
data-dependent in a way a log frozen at length 0-6 never was, which may
explain the noise on its own, or the groupArray fix's incidental
consistency may be doing some of that work too. Not investigated further
here -- worth knowing before anyone treats a single fold-in-isolation run
as precise.

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

**This ADR is amending ADR-0001's acceptance criterion.** 1,159 instr/sec
e2e (the final, verified number -- see the top of this document) is not a
number to leave quietly unmet against a >=10,000 threshold. The 10,000
figure was a pre-implementation estimate, made before Phase 0 discovered
that arrayFold's cost model is per-expression-node rather than
per-byte-of-data-moved -- a discovery that came *after* the number was
picked, not something the number accounted for. SPEC §1 correctness was
never optional either way. A throughput target set before the cost model was
understood, and before the correctness requirements were priced in, is the
thing that should move, not the correctness.

**Wall-clock consequence, stated plainly rather than left for discovery
during the timelapse run:** `-timedemo demo3` is 2,134 tics (read from the
WAD directly) at refemu's measured ~1.36M instructions/tic -- 2.91 billion
instructions. At 1,159 instr/sec that run is **~29 days** -- see the top of
this document. This is not proposed as the final number: §Consequences below
names the concrete, understood optimization (consolidating
`RESULT`/`NEXT`/`HALT_CODE` into one dispatch) that should be attempted
before this becomes the number DOOM actually runs at, and the batch-commit
overhead investigation (also in §Consequences) is a second, independent
lever of comparable size. Amending ADR-0001's threshold now is about not
blocking correctness on an estimate made before the cost model was known --
it is not a claim that 1,159 instr/sec is an acceptable place to stop. It
is, however, within 159 instr/sec of the human owner's standing 1,000
instr/sec escalation floor, which is not this ADR's to relax -- see the top
of this document.

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
