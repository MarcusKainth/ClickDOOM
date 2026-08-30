# Batch overhead

Splits the per-instruction cost that end-to-end pays over the fold in
isolation into its two candidate sources:

1. **State reload.** Reading the previous batch's pc, registers and icount, and
   materialising the fold's result into `batch_commit` with `INSERT ... SELECT`
   instead of returning it to the client. RAM and decode materialisation is
   identical on both sides and cancels out of the subtraction.
2. **Write-log flush.** The two statements the end-to-end loop adds after the
   batch INSERT: merging the write-log into `ram`, which scales with write-log
   length, and deriving the `cpu_state` row, which is O(1).

Both flushes come from the executor's own commit path rather than a stand-in,
so the measurement cannot drift from what a real flush does.

## Run

    make bench-batch-overhead

Creates and drops its own private database, so a concurrent run against the
shared one cannot corrupt it. It brackets its own `query_log` window and aborts
if it finds statements it did not issue, which is how it detects contention.

Override `CLICKDOOM_BENCH_K`, `CLICKDOOM_BENCH_BATCHES`, `CLICKDOOM_BENCH_HWM`
and `CLICKDOOM_BENCH_DB`.

## What it needs

A quiet machine. These are timings, and the effect being measured is smaller
than the noise floor on a busy box.
