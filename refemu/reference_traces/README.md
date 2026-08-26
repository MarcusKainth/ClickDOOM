# SPEC §7 reference traces

Committed oracle output for `sqlcpu`/`executor` to diff against, so a
divergence during Phase 2's integration run (or `just diff`'s future
`scripts/diff_run.sh`, issue #27) is caught at the instruction it happens,
not after hours of compute produce a final hash that doesn't match. See
`../scripts/gen_reference_trace.py`'s module docstring for the full
rationale, what this deliberately does and doesn't cover, and how to
regenerate (`just gen-reference-trace` from the repo root, after `just
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

This is the **third** ROM this directory has held a trace for:
`e133789d9cec…` (attract-mode, superseded by #111 before its trace was
ever merged) → `e74cf575f931…` (`-timedemo demo3`, superseded by #127) →
`eabb12ed4f18…` (current, frozen). The hash-in-filename convention is
exactly what makes each transition safe rather than a repeat of the
near-miss it was built to prevent after the first one.
