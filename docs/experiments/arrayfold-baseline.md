# Baseline arrayFold throughput

This is the characterisation measurement the SQL CPU was built on. It asks
whether `arrayFold` can carry a CPU step at all, what the accumulator does
with a large captured array, and what sets the price of one step.

## Question

Can one `arrayFold` over `range(K)` execute a RISC-V step expression fast
enough to be worth building on? If it can, which property of the step
expression decides the cost?

## Method

A synthetic fixture stands in for the ROM: 24 MiB of RAM (6,291,456 words),
2 MiB of text (524,288 pre-decoded instructions), and an instruction mix
shaped like DOOM's, roughly 35% ALU, 25% load, 10% store, 18% branch, 5%
jump and 7% M-extension. Every fixture value comes from a multiplicative
hash of `number`, so the fixture is identical on every run and no `rand()`
or `now()` is involved.

Timing is client-side wall clock around one `clickhouse-client` call. Three
things are measured separately: the fold on its own, the fold end to end
with state reload and write-log flush, and a set of variants that change one
property of the step expression at a time.

## Conditions

| | |
|---|---|
| Date | 2026-08-26 |
| ClickHouse | 26.3.17.4 |
| Machine | Apple Silicon, Docker Desktop |
| Settings | `max_threads = 1` |
| RAM fixture | 6,291,456 words (24 MiB) |
| Text fixture | 524,288 pre-decoded instructions (2 MiB) |

## Results

### The fold on its own

No state reload, no commit. Two runs per row.

| variant | K | seconds | instr/sec |
|---|---:|---:|---:|
| pre-decoded | 10,000 | 0.872 / 0.857 | 11,467 / 11,668 |
| pre-decoded | 50,000 | 3.794 / 3.745 | 13,178 / 13,351 |
| pre-decoded | 200,000 | 14.681 / 14.815 | 13,623 / 13,499 |
| decode inside the lambda | 10,000 | 6.333 / 6.480 | 1,579 / 1,543 |

Pre-decoding the instruction stream is worth 7.4x.

### The accumulator does not copy a captured array

Same fold, same K = 100,000, varying only the size of the captured RAM
array.

| captured RAM array | seconds | instr/sec |
|---|---:|---:|
| 1,024 words (4 KiB) | 0.878 | 113,895 |
| 65,536 words (256 KiB) | 0.864 | 115,740 |
| 1,048,576 words (4 MiB) | 0.914 | 109,409 |
| 6,291,456 words (24 MiB) | 0.935 | 106,951 |

Flat across a 6,144x range in array size, so a captured constant array is
not copied into the accumulator per step and `arrayElement` on it is
effectively constant time. Holding all 24 MiB of RAM as a query-level
constant is sound. The rate is high here because the fold body is
deliberately trivial, so this variant prices array capture rather than
instruction execution.

### End to end

State reload, fold, write-log flush into `ram`, `cpu_state` update, looped
over batches and wall-clocked.

| K | batches | instructions | seconds | instr/sec |
|---:|---:|---:|---:|---:|
| 10,000 | 60 | 600,000 | 68.797 | 8,721 |
| 50,000 | 12 | 600,000 | 50.444 | 11,894 |
| 200,000 | 3 | 600,000 | 51.598 | 11,628 |

K = 50,000 is the best of the three. At K = 10,000 the roughly 0.30 s
per-batch fixed cost is 30% of the batch, of which 0.15 s is materialising
the 24 MiB RAM constant and 0.017 s the decode arrays; by K = 50,000 it is
under 8%. Past K = 50,000 the write-log's superlinear growth cancels the
remaining amortisation. The acceptance threshold in force at the time was
10,000 instructions per second sustained end to end, and K = 50,000 clears
it.

### Cost per step against node count

Cost per fold step tracks the size of the expression in the lambda, not the
volume of data touched. N chained `bitXor` nodes at K = 20,000:

| nodes | seconds | us per node per step |
|---:|---:|---:|
| 2 | 0.071 | 1.77 |
| 20 | 0.270 | 0.68 |
| 100 | 1.260 | 0.63 |
| 400 | 7.881 | 0.99 |

The last column divides by node count, and that is the wrong denominator.
[`compiled-node-cost.md`](compiled-node-cost.md) reruns this shape on
26.7.5.10 with node count and distinct-literal count moved independently: a
node is 4.4 ns compiled and 0.29 us interpreted, and each distinct literal in
the chain costs 0.16 to 0.28 us per step. Figures in this column's range come
out of a chain whose literals move with its nodes, and they price the node and
its literal together.

`arrayFold` evaluates its lambda as a full expression pass over a one-row
block per element, so ClickHouse pays its per-function-call overhead on
every node. Five consequences follow, each measured rather than assumed.

- Neither `multiIf` nor `if` short-circuits. A 40-arm `multiIf` costs the
  same whether arm 0 or arm 39 matches, 1.648 s against 1.601 s, and
  `if(false, <400 nodes>, cheap)` costs what taking the expensive branch
  costs. Ordering arms by opcode frequency buys nothing.
- A binary dispatch tree is worse than a flat `multiIf`. Forty leaves at
  depth 6 measured 1.861 s against the flat form's 1.648 s, because the
  whole tree evaluates.
- Width is nearly free. Three register-write strategies, an `arrayMap`
  rebuild over 32 elements, bind-then-map, and `arraySlice` plus
  `arrayConcat`, landed within 2% of each other, because a 32-row block
  costs about what a 1-row block costs.
- `short_circuit_function_evaluation` does not rescue it. `enable`,
  `force_enable` and `disable` measured 6.385 s, 6.451 s and 6.4 s on the
  realistic fold. Those three are within about 1% of each other on this
  fixture and 24% apart on the production fold, measured below.
- The let-binding idiom `arrayMap(v -> ..., [expr])[1]` costs about 4.5 us
  per binding, so recomputing a cheap subexpression twice beats binding it
  once.

### The short-circuit setting on the production fold

The three settings separate on the production step expression. Measured on
2026-08-31 on 26.7.5.10 against the real ROM, boot window, K = 60,000,
HWM = 20,000, `max_threads = 1`, the production fold runs a batch in
15,328 ms at `enable`, 16,776 ms at `force_enable` and 13,544 ms at
`disable`. `force_enable` changes which islands the JIT builds, and
`CompiledFunctionExecute` over the batch falls from 2,460,000 to 1,439,977.

Paired inside one container over 6 pairs, `disable` against `enable` is
-11.7%, 95% interval [-12.05%, -11.39%], with byte-identical output over the
paired batch and over 720,000 chained instructions. That figure carries the
non-zero divisor guard the fold needs in order to run at `disable`, which
costs +1.56% on its own, so the setting by itself is worth -13.07%.

Both fold queries pin `short_circuit_function_evaluation = 'disable'` in
their own `SETTINGS` clause (`executor/src/fold.rs`).
`docs/adr/0002-predecoded-instruction-table.md` carries what an expression
has to satisfy to run under that pin.

### Where the remaining time goes

Ablation at K = 50,000 on the pre-decoded variant, against a 3.629 s
baseline.

| removed | seconds | share of total |
|---|---:|---:|
| M-extension arms | 2.962 | 18% |
| branch arms | 3.222 | 11% |
| store path | 3.248 | 10% |
| load path | 3.308 | 9% |
| write-log probe on loads | 3.453 | 5% |
| everything else | | about 52% |

There is no hotspot. Cost is diffuse. The M-extension arms are the largest
single group, and because nothing short-circuits they cost that 18% on every
instruction rather than only on multiplies.

## Verdict

`arrayFold` carries a CPU step, and the design proceeds on that basis.
Pre-decoding the instruction stream is worth 7.4x and is not optional.

The step expression sets the per-step price and the volume of data it touches
does not. Which property of the expression sets it is settled in
[`compiled-node-cost.md`](compiled-node-cost.md), and it is not node count.

## Limits

The fixture is pseudo-random words and a synthetic decode table, so nothing
here says the CPU is correct. The real instruction mix barely affects
throughput, since `multiIf` does not short-circuit, but it decides which
arms are worth collapsing. MMIO, frame commit and the render query are not
in the fold at the time of this measurement. The numbers come from one
developer machine.
