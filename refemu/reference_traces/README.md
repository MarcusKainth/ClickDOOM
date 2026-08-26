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

## Current status: no trace committed here yet

Held pending #111 (`-timedemo demo3` argv). The attract-mode ROM's trace
(generated, cross-checked against issue #29's independent reproduction,
and then deliberately not merged — see PR #114) is not committed here
because its ROM is about to stop being the one this project runs. Once
#111 lands, regenerate against the timedemo ROM's real `PINNED_HASH` with
`just gen-reference-trace`.
