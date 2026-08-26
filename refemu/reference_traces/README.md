# SPEC §7 reference traces

Committed oracle output for `sqlcpu`/`executor` to diff against, so a
divergence during Phase 2's integration run (or `just diff`'s future
`scripts/diff_run.sh`, issue #27) is caught at the instruction it happens,
not after hours of compute produce a final hash that doesn't match. See
`../scripts/gen_reference_trace.py`'s module docstring for the full
rationale, what this deliberately does and doesn't cover, and how to
regenerate (`just gen-reference-trace` from the repo root, after `just
build-rom`).

## `demo-boot-to-first-frame.tsv` / `.json`

The real DOOM ROM (`rom/PINNED_HASH`), boot to icount 13,631,488 — past the
first `FRAME_COMMIT` (icount 13,243,964, per issue #29) with one full
`RAM_HASH_INTERVAL` checkpoint of margin. `.tsv` is the SPEC §7 trace
itself (one line per `CHECKPOINT_INTERVAL`, `ramhash`/`fbhash` appended
every `RAM_HASH_INTERVAL`); `.json` is generation metadata — ROM sha256,
the exact command line, and two milestones that do **not** appear as
`.tsv` lines because they don't fall on a periodic-interval boundary
(`init_graphics_icount`, `frame_commit`).

Independently cross-checked against issue #29's own reproduction (a
different, ad hoc script, not this one) before being committed: both the
`I_InitGraphics` icount and the full first-`FRAME_COMMIT` checkpoint line
(icount, pc, reghash, ramhash, **fbhash `ce36be7a861e13e0`**) match
exactly. `fbhash` is the number that matters most here — it is what
"final-frame hash matches refemu" (README's victory condition) checks, and
it is blind to nothing upstream of rendering the way `reghash`/`ramhash`
alone would be (SPEC §7).

If a future ROM change moves `PINNED_HASH`, this trace goes stale —
`gen_reference_trace.py` refuses to regenerate silently against a
different binary (checks `rom/PINNED_HASH`, fails loudly on a mismatch)
and refuses to report a milestone that disagrees with the `--expect-*`
defaults baked into it without saying so.
