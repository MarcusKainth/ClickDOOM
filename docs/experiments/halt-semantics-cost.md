# What halt semantics cost the fold

The production fold carries halt checks, write-log versioning, and address,
alignment and self-modify checks. The baseline fold carries none of them.
This harness is the reproducible before-and-after between the two. It never
produced a recorded result.

## Question

What do the fold's halt semantics cost, in instructions per second, against
a baseline fold running the same synthetic instruction stream?

## Method

Both sides run the same synthetic instruction stream the baseline
measurement used, so the two numbers are comparable line for line against
the baseline's fold-in-isolation table.

The run creates and drops its own private database rather than using a
shared one, so a concurrent run cannot corrupt it mid-sweep. Every table's
DDL is generated from the real schema with the database name rewritten, and
only the synthetic instruction mix is added on top. `batch_commit`'s first
row is seeded by the production bootstrap script.

Two phases, swept over K:

- The fold in isolation. The production fold generator in its select-only
  form, timed with client-side wall clock around one `clickhouse-client`
  call.
- End to end. The production fold generator in its end-to-end form, followed
  each batch by the `ram` and `cpu_state` flushes from the production commit
  generators, looped over enough batches to execute 600,000 instructions and
  wall-clocked around the whole loop.

Output is TSV with variant, mode, K, seconds and instructions per second.

## Conditions

| | |
|---|---|
| Date | no result recorded |
| K | 10,000, 50,000 and 200,000 by default |
| Repeats | 2 by default |
| HWM | 20,000 by default |
| Database | private, created and dropped by the run |

## Results

No result was ever recorded for this harness. It has a runnable method and
no numbers.

The before-and-after it exists to reproduce was measured separately, at
K = 50,000 on ClickHouse 26.3:

| | baseline fold | fold with halt semantics |
|---|---:|---:|
| fold in isolation | 76 us/instr | 537.5 us/instr |
| end to end | 84 us/instr | 862.6 us/instr |
| end to end over fold ratio | 0.90 | 0.62 |

That is about 1,860 instructions per second for the fold in isolation and
1,159 end to end. Those numbers did not come from this harness.

## Verdict

None. The comparison was never measured here.
