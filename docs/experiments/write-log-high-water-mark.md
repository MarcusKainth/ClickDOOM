# The write-log high-water mark

The batch contract requires a write-log high-water mark that ends a batch
early. This measurement sets it. The value it produced,
`WRITE_LOG_HIGH_WATER_MARK_DEFAULT = 20_000` in `executor/config.py`, is
still the default, and this record is its provenance.

## Question

Does per-step cost bend as the write-log grows, and at what write-log length
does the bend start?

## Method

The worst case: every instruction in the batch is a store, each to one of a
small cycling pool of addresses just past a tiny 8,192-word decode window,
so `arrayPushBack` grows the write-log's arrays by exactly one entry on
every step for the full length of the batch. The high-water mark is set
above K so it never triggers, and the fold runs in its select-only form, so
what is timed is the write-log's own growth cost and nothing else. K is
swept and the per-step cost read off each arm.

Real DOOM code is not this store-dense, so this is deliberately the worst
case rather than the expected one.

## Conditions

| | |
|---|---|
| Date | 2026-08-26 |
| ClickHouse | 26.3.17.4 |
| Settings | `max_threads = 1` |
| Store density | 100%, every instruction a store |
| Decode window | 8,192 words |

Timing is client-side wall clock around one `clickhouse-client` call.

## Results

| K, all stores | seconds | us per step |
|---:|---:|---:|
| 2,500 | 0.775 | 310.00 |
| 5,000 | 1.188 | 237.60 |
| 10,000 | 2.039 | 203.90 |
| 20,000 | 3.606 | 180.30 |
| 40,000 | 7.503 | 187.57 |
| 80,000 | 17.590 | 219.88 |
| 160,000 | 43.371 | 271.07 |

Per-step cost falls as fixed overhead amortises, bottoms out around
K = 20,000 to 40,000, then climbs: 20% worse by 80,000 and 50% worse by
160,000. The mechanism is the write-log's own length, through the
`arrayLastIndex` scan on every load and the `arrayPushBack` copy on every
store, rather than K as such.

### The realistic case

The all-stores curve is not the shape the real ROM produces. Seeding the
write-log directly, which varies log length independently of K, gives the
mixed-instruction case:

- General-RAM store density in the boot window is 33.3%, so 20,000 write-log
  entries accumulate in 60,006 instructions. At the production K = 60,000
  the log reaches 19,998 against a mark of 20,000, six instructions below
  early termination. The mark is a live constraint on the boot window, not
  only a safety valve for anomalous code.
- The bend is real but much gentler than the all-stores curve suggests. Per
  write-log element per step the fold costs 3.41 ns, with a 95% confidence
  interval of 3.27 to 3.61 ns, and the slope divided by K is constant to 7%
  across K = 15,000 to 60,000, so the cost is linear in log length rather
  than in K. The load-forwarding scan is 81% of that and the accumulator
  copy 19%.

[`write-log-growth.md`](write-log-growth.md) is that measurement.

## Verdict

The high-water mark default is 20,000, at the bottom of the measured
worst-case curve and comfortably before the bend.

Its true optimum against the real ROM is about 17,960, corresponding to
K ≈ 53,900, worth 0.08%, which is inside the noise. The choice made against
a worst case survives being measured on a different window. Deleting the
load-forwarding scan altogether would be worth 6.4%, which is the ceiling on
any write-log restructuring.

Because the boot window runs about six instructions below the mark, a ROM
change that raises store density even slightly starts truncating every boot
batch. Each truncation costs a full batch setup of about 1.65 s, which would
read as a sudden throughput drop of roughly 6% with no code change to blame
it on.
