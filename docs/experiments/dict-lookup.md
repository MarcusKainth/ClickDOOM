# dictGet against arrayElement for RAM reads

The fold reads RAM out of a captured constant array. A ClickHouse dictionary
is the obvious alternative, because it is loaded once per server rather than
captured once per query. This prices both.

## Question

Would a `FLAT` or `HASHED` dictionary beat the captured constant array the
fold reads RAM from?

## Method

A dense table of 6,291,456 words backs both sides. Each arm runs an
`arrayFold` that performs a fixed number of RAM reads per step, either
through `arrayElement` on a captured array or through `dictGet`. A `floor`
arm with no read and a capture-only arm at K = 1 separate the per-read cost
from the per-batch fixed cost. Two further arms price what each side pays to
see a batch's stores: `SYSTEM RELOAD DICTIONARY` for the dictionary, and
re-capturing the array for the array.

## Conditions

| | |
|---|---|
| Date | 2026-08-29 |
| ClickHouse | 26.7.5.10 |
| Machine | fresh container |
| Settings | `max_threads = 1`, `compile_expressions = 0` |
| K | 20,000 |
| RAM table | 6,291,456 words, dense |
| Repeats | 3, best reported |

## Results

| variant | seconds |
|---|---:|
| floor, K = 20,000 | 0.090 |
| capture the array only, K = 1 | 0.128 |
| `arrayElement`, one read per step | 0.264 |
| `dictGet` FLAT, one read per step | 0.166 |
| `dictGet` HASHED, one read per step | 0.160 |
| `arrayElement`, four reads per step | 0.394 |
| `dictGet` FLAT, four reads per step | 0.329 |
| `SYSTEM RELOAD DICTIONARY` after 20,000 stores | 0.064 |
| capture the array after 20,000 stores, K = 1 | 0.183 |

Per read, after subtracting the floor and the capture, `arrayElement` costs
about 2.2 us and `dictGet` about 3.0 to 3.8 us. Per batch, the dictionary
saves the capture, 0.12 to 0.18 s, and costs a reload, 0.06 s.

A read through a dictionary is 1.4x slower per node than a read from the
captured array, and the dictionary's fixed cost per batch is 0.12 s lower.
At 60,000 steps and about one RAM read per step the two effects are within
0.1 s of each other, under 1% of a 16 s batch either way.

## Verdict

No lever. A dictionary changes where a lookup node's time goes, not how much
of it there is. The fold keeps the captured array.
