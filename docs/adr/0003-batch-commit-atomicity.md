# ADR-0003: Batch commit atomicity via a single atomic row plus idempotent derivation

**Status:** proposed — gated on human ratification of the accompanying
`spec:` change (issue #35) and `sqlcpu` agreement on the `batch_commit` DDL.

## Context

SPEC §6 requires batch commit to be atomic: "either all effects (ram deltas,
cpu_state row, MMIO side effects) land or none do." ClickHouse has no
cross-table atomic write. The naive shape — flush the write-log into `ram`,
then insert a `cpu_state` row, then apply MMIO side effects as further writes
— has a real crash window: a process death between any of those steps leaves
`ram` and `cpu_state` disagreeing about which instructions have "happened",
and the next batch would read a mix of pre- and post-batch state.

Phase 0 didn't resolve this (it wasn't in scope); it surfaced during planning
for #25.

## Decision

Shrink "atomic" to exactly the one thing that actually is atomic in
ClickHouse — a single-row `INSERT` — and make everything else idempotent
derivation from that row, safe to redo any number of times.

1. **`batch_commit` is the atomic write.** One `INSERT` of one row per batch,
   carrying `cpu_state`'s columns plus everything recovery needs: the
   write-log (`wl_addr`/`wl_val`/`wl_icount`), cumulative `keyq_pos`,
   `console_bytes`, and frame-commit info. A single-row `INSERT` is one block,
   one part — ClickHouse's actual atomicity primitive, not an assumption.
2. **`cpu_state` becomes a view** over `batch_commit`, projecting exactly
   SPEC §5's existing shape. No consumer of `cpu_state` needs to change.
3. **`ram` flush is idempotent, not atomic-with-(1).** `INSERT INTO ram
   SELECT ... FROM batch_commit WHERE batch_id = <latest>`. Because `ram` is
   a `ReplacingMergeTree` keyed by `word_addr`/`version`, and each delta's
   version is now the store's own `icount` (not the batch's final `icount` —
   see the versioning fix filed against PR #30), re-running this flush after
   a partial or interrupted attempt is always safe: duplicate rows for an
   already-applied delta carry the same value, so `FINAL` resolves to the
   same answer regardless of how many times the flush ran. Recovery is
   therefore just "unconditionally redo the flush for the latest
   `batch_commit` row before running any new batch" — no state machine, no
   partial-apply bookkeeping.
4. **`console_out` gets the same idempotent-flush treatment as `ram`** — an
   append with a deterministic, dedup-safe key, not a fragile "have I already
   appended this" check.
5. **`input_queue` consumption needs no write of its own.** Whether a queued
   event has been consumed is a computed predicate — its rank by `event_seq`
   compared against `batch_commit`'s cumulative `keyq_pos` — not a mutated
   flag. The cheapest way to make a write atomic is to not have the write.
6. **Bulky columns are bounded, not retained forever.** `wl_*` and
   `console_bytes` on `batch_commit` are only ever read for the
   most-recently-committed row in normal operation (everything older has
   already been flushed into `ram`/`console_out`), so they carry a TTL
   (executor/config, default 1 day) rather than accumulating for the life of
   a `demo3` run (~80,000 batches, which would otherwise retain gigabytes of
   write-log arrays with no further use). The small columns (`batch_id`
   through `exit_code`) are cheap and kept forever, matching §5's "one row
   per committed batch" for `cpu_state`.

## Consequences

- Satisfies SPEC §6 as clarified: no external observer, including a §7
  differential trace, can ever witness a half-applied batch, whether or not
  a crash interrupted derivation — because the only fact that can be
  "half-applied" is a single-row insert, which by construction cannot be.
- **Documented tradeoff, not a solved problem:** if the driver is down longer
  than the TTL window before restarting, the last committed batch's
  write-log can expire before recovery replays it, silently losing those
  deltas from `ram` going forward. Judged acceptable for this project's
  supervised-run operating model (a human-run timelapse, not an unattended
  service); would need revisiting for that use case.
- Requires a SPEC §5 change (`batch_commit` table, `cpu_state` as a view) —
  additive only, no existing shape changes. Filed as issue #35, human-gated
  per CODEOWNERS.
- `sqlcpu` owns `schema.sql` and must agree the DDL before this lands;
  coordination is happening on issue #25 and directly with `sqlcpu`.
- Expected throughput impact: near zero on the fold itself (the persisted
  fields are already produced by the accumulator per #23/#24, not new
  computation); the flush queries are structurally the same shape Phase 0
  already measured at 11,894 instr/sec e2e. Validated with a before/after
  benchmark in the implementation PR, and with a crash-recovery test using
  SPEC §7's `RAM_HASH_INTERVAL` checkpoint hash as the correctness oracle.
