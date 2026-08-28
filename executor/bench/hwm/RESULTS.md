# Write-log high-water mark: measured, not guessed

SPEC §6 requires a write-log high-water mark that ends a batch early. Phase 0
didn't set one (its e2e harness never accumulated enough stores in one batch
to matter). This measures the curve `RESULTS.md` in Phase 0 promised: does
per-step cost bend as the write-log grows, and where.

## Method

Worst case: every instruction in the batch is a store (`op_id=19`), each to
one of a small cycling pool of addresses just past a tiny (8192-word) decode
window -- so `arrayPushBack` grows the write-log's arrays by exactly one
entry every single step, for the full length of the batch. Real DOOM code
won't be this store-dense (Phase 0's synthetic DOOM-shaped mix used ~10%
stores), so this is intentionally the worst case, not the expected case.
`fold.select_only` with the high-water mark set above `K` (so it never
triggers) isolates just the write-log's own growth cost.

Reproduce: `executor/bench/hwm/` — schema and fold-generator scripts, timed
the same way as `executor/bench/phase0/run.sh` (client-side wall clock around
one `clickhouse-client` call, ClickHouse 26.3.17.4, `max_threads=1`).

## Result

| K (all stores) | seconds | µs/step |
|---:|---:|---:|
| 2,500 | 0.775 | 310.00 |
| 5,000 | 1.188 | 237.60 |
| 10,000 | 2.039 | 203.90 |
| 20,000 | 3.606 | **180.30** |
| 40,000 | 7.503 | 187.57 |
| 80,000 | 17.590 | 219.88 |
| 160,000 | 43.371 | 271.07 |

Per-step cost falls as fixed overhead amortizes, bottoms out around
K = 20,000-40,000, then climbs -- 20% worse by 80,000, 50% worse by 160,000.
This is the same "write-log's superlinear growth" SPEC §6 already names as
the reason K itself is capped at 50,000; this measurement is the same effect
showing up as a function of write-log length specifically (via
`arrayLastIndex`'s scan on load, and the `arrayPushBack` copy on store)
rather than of K.

## Decision

**High-water mark default: 20,000** (`executor/config.py`,
`WRITE_LOG_HIGH_WATER_MARK_DEFAULT`) -- at the bottom of the measured curve,
comfortably before the bend. This is a worst-case-store-density number: at
Phase 0's ~10% store fraction, a batch would need roughly 200,000
instructions of nothing but stores to reach it, well past K's own 50,000
cap -- so in ordinary operation the mark is a safety valve for anomalous
code, not a normal-path constraint, consistent with SPEC §6 treating it as
one of three early-termination conditions alongside halt and `FRAME_COMMIT`.

## What this does not measure

The realistic mixed-instruction case (only ~10% stores) at high write-log
lengths -- not reproduced here since reaching a 20,000-entry log at 10%
store density needs a batch far larger than K's own 50,000 cap, i.e. it
cannot happen inside one batch under the current K/§6 rules. If K or the
store fraction changes later, this curve should be re-measured rather than
assumed to still apply.

## Filled in by #257 -- and the paragraph above was wrong about the real ROM

**Read that gap as closed.** `executor/bench/wl_seed` measured the realistic
case against the real ROM by seeding the write-log directly, which varies log
length independently of K and so does not need a batch larger than K's cap.
Numbers and provenance are on **#257**.

Two corrections to what is written above, both measured rather than argued:

- **The ~10% store fraction does not hold in the boot window.** Real
  general-RAM store density there is **33.3%** (20,000 write-log entries in
  60,006 instructions, #180), not 10%. So the mark is reached in ~60,000
  instructions, not the ~200,000 this file estimated -- and it is therefore
  **not** merely "a safety valve for anomalous code". At the production
  K = 60,000 the log reaches **19,998** against a mark of 20,000. Boot runs
  about six instructions below the truncation point.
- **The bend is real but much gentler than the all-stores curve suggests.**
  Per write-log element per step the fold costs **3.41 ns** (95% CI
  [3.27, 3.61]), and `slope/K` is constant to 7% across K = 15,000-60,000, so
  the cost is linear in log length rather than in K. Of that, **81% is the
  load-forwarding `arrayLastIndex` scan** and 19% the accumulator copy.

**The 20,000 default stands.** Its true optimum against the real ROM is
HWM ~ 17,960 (K ~ 53,900), worth **0.08%** -- comfortably inside the noise,
and this file's choice was made against a worst case rather than this window,
which is why it survives being measured on a different one. Deleting the scan
altogether would be worth 6.4%, which is the ceiling on any write-log
restructuring.

`executor/config.py`'s `WRITE_LOG_HIGH_WATER_MARK_DEFAULT` still cites this
file, correctly: the constant is unchanged and this remains its provenance.
