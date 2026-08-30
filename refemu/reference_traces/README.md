# SPEC §7 reference traces

Committed oracle output for `sqlcpu`/`executor` to diff against, so a
divergence during Phase 2's integration run (or `make diff`'s future
`scripts/diff_run.sh`, issue #27) is caught at the instruction it happens,
not after hours of compute produce a final hash that doesn't match. See
`../scripts/gen_reference_trace.py`'s module docstring for the full
rationale, what this deliberately does and doesn't cover, and how to
regenerate (`make gen-reference-trace` from the repo root, after `just
build-rom`).

## Naming: `demo-boot-to-first-frame.<rom sha256 prefix>.tsv` / `.json`

Each trace's filename carries a 12-hex-char prefix of the ROM's own sha256
(matching `git`'s short-hash length) — not just the `.json` sidecar's
`rom_sha256` field. This is deliberate, not decorative: this directory's
first real trace was generated against the attract-mode ROM
(`e133789d9cec…`) hours before issue #111 (wiring `-timedemo demo3` argv,
README's actual victory-condition invocation) made that ROM obsolete and
moved `PINNED_HASH`. A same-named file would have made the stale trace
silently *look* current; a teammate happening to notice a mismatched
instruction count in a status message is what actually caught it. The
hash-in-filename convention is the fix that doesn't depend on a human
noticing a second time.

`.tsv` is the SPEC §7 trace itself (one line per `CHECKPOINT_INTERVAL`,
`ramhash`/`fbhash` appended every `RAM_HASH_INTERVAL`); `.json` is
generation metadata — ROM sha256, the exact command line, and any
milestones (e.g. `I_InitGraphics` reached, first `FRAME_COMMIT`) that
don't fall on a periodic-interval boundary and so can't be `.tsv` lines.

`gen_reference_trace.py` refuses to generate against a ROM that doesn't
match `rom/PINNED_HASH` (fails loudly, not silently), and refuses to
report a milestone that disagrees with its `--expect-*` defaults without
saying so — a mismatch there means either the ROM genuinely changed
(update the defaults) or refemu itself regressed (investigate before
trusting the output).

## `demo-boot-to-first-frame.eabb12ed4f18.tsv` / `.json`

The real DOOM ROM, frozen (#127/#130's ROM freeze) at
`PINNED_HASH eabb12ed4f188f456177fc11a1fdcf3046ee5c9c38c8d2fd33246c72bd2ab92c`
(`-timedemo demo3` argv from #111, `DG_DrawFrame` unrolled x8 by #127),
boot to icount 15,728,640 — the first full `RAM_HASH_INTERVAL` checkpoint
at or past the first `FRAME_COMMIT` (icount 15,653,137, #29/#127).

Independently cross-checked against issue #29's own reproduction before
being committed: `I_InitGraphics` reached at icount 11,016,543 (unchanged
from pre-#127 — that unroll only touches `DG_DrawFrame`, well after
`I_InitGraphics`), and the full first-`FRAME_COMMIT` checkpoint line
(icount, pc, reghash, ramhash, **`fbhash fe5d82c0f42d45f1`**) match
exactly. `fbhash` **unchanged** from the pre-#127 ROM is the whole point of
that PR — same rendered frame, fewer instructions to produce it — and this
regeneration confirms it holds for refemu's own trace, not just the
one-off comparison in #127's review.

This was the **third** ROM this directory held a trace for:
`e133789d9cec…` (attract-mode, superseded by #111 before its trace was
ever merged) → `e74cf575f931…` (`-timedemo demo3`, superseded by #127) →
`eabb12ed4f18…` (frozen, superseded by #175 below). The hash-in-filename
convention is exactly what makes each transition safe rather than a
repeat of the near-miss it was built to prevent after the first one.

## `demo-boot-to-first-frame.9a6a47d01119.tsv` / `.json`

The **fourth** ROM, `PINNED_HASH 9a6a47d01119f67580e48e9875207186c25efd56ff93019df331eb307cfaa5d9`
(#175: id Software's own dormant, unused-since-1993 8x/4x loop-unrolled
`R_DrawColumn`/`R_DrawSpan` — 62.28% of the entire `demo3` instruction
count between the two of them — enabled for real, human-owner-approved
conditional on frame-hash equivalence). Boot to icount 15,728,640, first
`FRAME_COMMIT` at icount **15,393,136** (was 15,653,137 — **260,001 fewer
instructions**, matching `rom-2`'s independent equivalence-gate finding
exactly and confirmed here a second way).

**`fbhash fe5d82c0f42d45f1` — unchanged from the previous ROM.** Same
significance as #127's unroll before it: same rendered frame, fewer
instructions to produce it, not a coincidence. `rom-2`'s own equivalence
gate confirmed this bit-for-bit across the first 300 committed frames
before this ROM was ever built for real; see the `demo3/` entry below for
the full-run (2,172-frame) confirmation this regeneration also produced
for free.

`I_InitGraphics` also shifted slightly: 11,016,543 → 11,014,966 (−1,577
instructions), despite that boot path never calling either unrolled
function directly — an ordinary knock-on effect of the binary's overall
size/layout changing (e.g. BSS zero-fill length, static data placement),
not a correctness concern for a boot-time console-output milestone.

## `demo3/demo3.eabb12ed4f18.json`

The full `-timedemo demo3` run, issue #129's harness (`../scripts/
gen_demo3_trace.py`), against the same frozen `eabb12ed…` ROM. **Only the
manifest is committed** — the `.tsv` trace itself (2,836,207,097
instructions, ~700K `CHECKPOINT_INTERVAL` lines, ~25 MB) is derived data,
gitignored, regenerable with `uv run python scripts/gen_demo3_trace.py`
in ~47 minutes on this machine (team lead's call, same reasoning as the
`demo-boot-to-first-frame` vs. this file's size difference above, just
applied at the far end of the scale).

**What this run established, none of it known before:**

- **The true `demo3` instruction count: 2,836,207,097** — replacing the
  extrapolation (2,134 tics × ADR-0004's ~1.36M/tic ≈ 2.90B, or × the E7
  subagent's ~1.47M/tic ≈ 3.14B) every planning figure in #104/#110/#147
  had been reasoning from. The measured figure is *below* both estimates
  (≈1.33M instructions/tic against 2,134 tics).
- **Termination is a clean `EXIT`, `exit_code = 4294967295`** — confirmed
  after a *full* demo, not just the isolated probe #107/#111 verified the
  mechanism with. Matches issue #121's proposed pinned value exactly.
- **The final frame hash: `fbhash d303721d8116e877`** — the literal
  artifact README's Definition of Victory requires the SQL CPU to
  reproduce. This is `final_state_at_halt` in the manifest, computed
  directly from the CPU state at the instant `EXIT` fired (icount
  2,836,207,097, `pc 0x800006b0` — the same `_exit` MMIO-write-then-spin
  address the #107/#111 probe found), **not** read off the `.tsv`'s own
  periodic cadence: the halt icount doesn't land on a `RAM_HASH_INTERVAL`
  boundary, so the trace's last hash-bearing line is stale by up to one
  interval. `gen_demo3_trace.py` computes and records this explicitly now
  (it didn't on the first real run — recovered manually from the saved
  resume-state, then fixed in the harness so future runs don't need that).
  2,172 frames were committed total; the last real `FRAME_COMMIT` was
  18,014 instructions before `EXIT`, with nothing writing to FRAMEBUFFER/
  PALETTE in between, so this `fbhash` is the last rendered frame's.

## `demo3/demo3.9a6a47d01119.json`

The full `-timedemo demo3` run against #175's ROM (`PINNED_HASH
9a6a47d01119…`): id Software's own dormant, unused-since-1993
loop-unrolled `R_DrawColumn`/`R_DrawSpan` enabled for real, 62.28% of the
prior run's entire instruction count between the two functions. Same
harness, same "only the manifest is committed" reasoning as the
`eabb12ed4f18…` entry above.

**The true `demo3` instruction count under the unroll: 2,300,210,133** —
**535,996,964 fewer instructions, an 18.90% cut**, above #175's own
conservative ~15.9% estimate (which reasoned from a 15-20% *per-function*
reduction, well below #127's precedent of ~53% on a structurally similar
unroll, applied only to the 62.28% of the run those two functions cover).

**`fbhash d303721d8116e877` — unchanged from the pre-#175 run.** This is
the equivalence claim's strongest form: `rom-2`'s own pre-implementation
gate confirmed bit-for-bit agreement across a validated representative
window (frames 0-299, 300 of 2,172); this run is the **entire** real
`demo3` — same `frame_commit_count` (2,172), same final `EXIT`/`pc
0x800006b0`/`exit_code 4294967295` as the pre-#175 baseline, and the same
final rendered frame despite arriving there in 18.90% fewer instructions.
30 years of dead code, one `#if 0`/`#endif` and a one-character typo away
from real, verified end to end rather than assumed correct because it
compiled.
