# What ClickHouse's expression JIT compiles in the fold step

ClickHouse can compile expressions to machine code with
`compile_expressions = 1`. This measures which constructs in the fold's step
expression it can compile, how a construct it cannot compile affects the
rest, and what the compilation is worth on the real step expression.

## Question

Which constructs block compilation? Does one non-compilable node disqualify
the whole enclosing subtree, or only itself? How much of the step expression
is handed to LLVM, and what does that buy?

## Method

Ground truth is `system.query_log`'s ProfileEvents rather than wall clock
alone. `CompileFunction` counts LLVM functions built,
`CompileExpressionsMicroseconds` the time spent in LLVM, and
`CompileExpressionsBytes` the generated code size at 8 KiB granularity.

Every probe is the same sandwich inside an `arrayFold` lambda:

    sink = CHAIN(links/2, CONSTRUCT( CHAIN(links/2, toUInt64(acc.3)) ))

`CHAIN` is a nest of unambiguously compilable UInt64 arithmetic, four
function nodes per link, with no casts and no type change, so nothing can
fragment it except the construct under test. The construct takes the lower
chain's value and the upper chain continues from its result, so a construct
LLVM cannot compile must cut the chain. Read against `chain_only`, which
gives exactly one island: one island means the construct is compilable and
fuses; two means it is not compilable but cuts only itself out; zero would
mean it poisons the whole expression.

`min_count_to_compile_expression = 0` moves compilation to the first
execution, so one run per variant settles the island counts. The timing
series uses the default of 3 and at least 6 repeats, so the step change on
the 4th run shows as a series rather than an average.

A third pass runs the production fold generator unmodified against this
experiment's own database, so the real step expression is measured rather
than a stand-in.

Two traps the harness had to design around, both worth carrying forward:

- `CompileFunction` is a cache-miss counter, not an island counter.
  ClickHouse's compiled-expression cache is server-global and keyed by each
  island's DAG rather than its SQL text, so rerunning an already-measured
  variant reports `CompileFunction = 0`. Every variant's literals are
  therefore salted, and the real-fold pass insists on a container whose
  cache is cold.
- A helper that references its argument twice explodes. The first draft of
  the fragmentation helper referenced its argument twice, which at 23
  insertions is more than eight million copies and a 1.9 GB SQL file. Every
  construct references its argument at most linearly.

## Conditions

| | |
|---|---|
| Date | 2026-08-26 |
| ClickHouse | 26.3.17.4 |
| Machine | private container with a cold compiled-expression cache |
| Settings | `max_threads = 1` |
| K | 100,000 for the probes, 100 and 100,100 for the real-fold pairs |

`marg` below is the median minus the near-empty `floor` step at the same K.
The floor costs 0.85 s to 1.03 s at K = 100,000 and never changes with any
setting.

## Results

### What compiles

At K = 100,000 with 24 chain links, on a cold cache.

Compilable, fusing into the surrounding chain: `plus`, `multiply`,
`bitAnd`, `bitOr`, `bitXor`, `bitShiftLeft`, `bitShiftRight`, every integer
cast (`toUInt8`, `toUInt32`, `toUInt64`, `toInt32`, `toInt64`), floating
`divide`, `least`, `greatest`, `abs`, `negate`, the comparisons, `and`,
`or`, `not`, `if`, and `multiIf` at 4, 12 and 28 arms. The 28-arm case is
9,064 AST nodes and is the shape the step expression's result and
next-state arms take.

Not compilable, cutting the chain: `intDiv` and `modulo`, which trap on
division by zero so LLVM lowering is refused; `transform`; `tuple`
construction; `arrayElement` on the register array, on the captured decode
table and on a plain literal array; `arrayLastIndex` with a lambda;
`arraySlice` with `length`; `arrayConcat`; `arrayPushBack`; and
`map(...)[k]`.

### A non-compilable node does not poison its parent

Three placements, all at 24 chain links.

| variant | placement | islands | reading |
|---|---|---:|---|
| `sib_arrayelem` | sibling, `plus(CHAIN, arrayElement(regs, 7))` | 1 | the parent `plus` compiles anyway, with the array read as an input |
| `sib_intdiv` | sibling, `plus(CHAIN, intDiv(acc.3, 3))` | 1 | same |
| `sib_tupleelem` | sibling, `plus(CHAIN, acc.3)` | 1 | same |
| `sib_arraylength` | sibling, `plus(CHAIN, length(regs))` | 1 | same |
| `nc_bot` | at the leaf, under 24 links | 2 | the chain above still compiles |
| `nc_top` | at the root, above 24 links | 2 | the chain below still compiles |
| `arrayelem_regs` | mid-chain, in path | 2 | chain above and chain below both compile |

A non-compilable node becomes an input to the compiled function around it.
It never disqualifies a parent, a sibling or an ancestor. The failure mode
is fragmentation into many small islands.

### Fragmentation is a mild tax

24 chain links with one non-compilable node inserted every `every` links, so
the island count is `24/every`. Medians of 3 runs at `compile_expressions` 0
and 1.

| variant | islands | marg at 0 (ms) | marg at 1 (ms) | speedup |
|---|---:|---:|---:|---:|
| `frag_none` | 1 | 8,865 | 2,968 | 2.99x |
| `frag_12` | 2 | 9,371 | 3,232 | 2.90x |
| `frag_8` | 3 | 9,686 | 3,385 | 2.86x |
| `frag_6` | 4 | 9,649 | 3,577 | 2.70x |
| `frag_4` | 6 | 10,117 | 3,950 | 2.56x |
| `frag_3` | 8 | 10,618 | 4,181 | 2.54x |
| `frag_2` | 12 | 11,762 | 4,707 | 2.50x |
| `frag_1` | 24 | 15,650 | 6,714 | 2.33x |

Shredding one chain into 24 four-node islands costs 22% of the compiler's
benefit, from 2.99x to 2.33x.

### Compile time against island size

A single island of N chain links, about 4 compilable function nodes each.

| links | function nodes | AST nodes | compile us | compile bytes |
|---:|---:|---:|---:|---:|
| 1 | 4 | 131 | 3,173 | 8,192 |
| 2 | 8 | 143 | 4,289 | 8,192 |
| 4 | 16 | 167 | 4,694 | 8,192 |
| 8 | 32 | 215 | 5,108 | 8,192 |
| 16 | 64 | 311 | 6,410 | 8,192 |
| 32 | 128 | 503 | 9,406 | 8,192 |
| 64 | 256 | 887 | 15,583 | 16,384 |
| 128 | 512 | 1,655 | 26,540 | 16,384 |

About 3.2 ms fixed per island plus about 46 us per compiled function node.
At 256 links and above the server's 8 MiB analyzer stack is exhausted with a
`TOO_DEEP_RECURSION` that no setting raises.

### The step change on the 4th run

Per-run series at K = 100,000, `compile_expressions = 1`, fresh salt so the
cache is cold.

| run | `floor` ms | `frag_none` ms | islands | `frag_4` ms | islands |
|---:|---:|---:|---:|---:|---:|
| 1 | 855 | 9,856 | 0 | 10,761 | 0 |
| 2 | 945 | 10,272 | 0 | 10,847 | 0 |
| 3 | 965 | 9,859 | 0 | 11,107 | 0 |
| 4 | 918 | 3,880 | 1 (8,402 us) | 4,623 | 6 (17,384 us) |
| 5 | 888 | 3,917 | 0 | 4,933 | 0 |
| 6 | 936 | 3,842 | 0 | 4,524 | 0 |

`CompileFunction` fires on exactly the run where the wall clock steps, and
reads 0 on runs 5 and 6 even though those runs are the fastest, which is
what a cache-miss counter does.

### Work the compiler cannot touch

`rw_N` adds N copies of the fold's per-step register-file rewrite,
`arrayConcat(arraySlice(regs, 1, rd-1), [v], arraySlice(regs, rd+1))`, on
top of an unfragmented 24-link chain. Those functions are non-compilable and
allocate a fresh 31-element array every step. Medians of 3 at K = 100,000.

| variant | rewrites per step | marg at 0 (ms) | marg at 1 (ms) | speedup |
|---|---:|---:|---:|---:|
| `rw_none` | 0 | 7,622 | 1,319 | 5.78x |
| `rw_1` | 1 | 8,417 | 1,956 | 4.30x |
| `rw_2` | 2 | 9,163 | 2,546 | 3.60x |
| `rw_4` | 4 | 10,945 | 3,765 | 2.91x |

One register-file rewrite per step, which is what the fold does once per
instruction, costs a third of the compiler's benefit. Four cost half. The
rewrite is 10% of the interpreted time and 33% of the compiled time.

Do not compare absolute speedups across these two families. `rw_none` at
5.78x and `frag_none` at 2.99x are both unfragmented 24-link chains with
identical AST node counts and they reproducibly differ, measured back to
back in one window at marg 7,125 against 9,211 interpreted and 1,312 against
2,709 compiled. The only difference is that `frag_none` uses 24 distinct
link constants while `rw_none` reuses 12 twice, which plausibly changes how
many 64-bit immediates LLVM has to materialise on aarch64. Each family is
internally consistent and run back to back, which is what its own curve
needs.

### The real step expression

The production generator, unmodified, on a private container.

| | value |
|---|---:|
| AST nodes (`EXPLAIN AST`, K = 2,000) | 103,816 |
| lambda body | 324,444 bytes |
| `CompileFunction`, cold cache | 58 |
| `CompileExpressionsMicroseconds`, cold | 204,576 |
| `CompileExpressionsBytes`, cold | 499,712 |
| `CompileFunction`, warm cache | 0 |

Against the calibration above, 204,576 us over 58 islands is about 3.5 ms
per island, at or below the cost of a four-function-node island. So the step
expression compiles into 58 very small islands, half a megabyte of machine
code and 205 ms of LLVM work.

What that buys is not measurable. Paired runs at K = 100 for fixed cost only
and K = 100,100, interleaved so machine load is common-mode, n = 3, times in
milliseconds.

| run | fixed j0 | fixed j1 | total j0 | total j1 | marg j0 | marg j1 | j0 - j1 |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 26,133 | 25,278 | 82,521 | 82,295 | 56,388 | 57,017 | -629 |
| 2 | 24,632 | 24,824 | 84,904 | 81,881 | 60,272 | 57,057 | +3,215 |
| 3 | 25,173 | 25,540 | 85,995 | 86,435 | 60,822 | 60,895 | -73 |

| | `compile_expressions = 0` | `compile_expressions = 1`, warm |
|---|---:|---:|
| marginal ms per 100,000 instructions | 59,161 (sd 2,417) | 58,323 (sd 2,228) |
| throughput | 1,690 instr/sec | 1,715 instr/sec |

The paired difference is 838 ms on about 59,000 ms, so 1.4%, with a 95%
confidence interval of -4,324 to +5,999 ms. That is a speedup of 1.014x with
a 95% interval of roughly 0.93x to 1.11x. Any real effect on the real step
expression larger than about 10% is excluded by this data.

Fixed per-batch cost at K = 100 against this 2 MiB fixture is 25.3 s,
independent of the compiler and much larger than anything measured here.

### Where the non-compilable work is

Textual site counts over the emitted lambda body of 324,444 bytes. The DAG
shares these heavily, so these are not node counts, but they show the shape.

| non-compilable | sites | compilable | sites |
|---|---:|---|---:|
| decode-table reads | 3,052 | integer casts | 8,324 |
| register-array reads | 835 | bit operations | 3,331 |
| `arrayPushBack` | 10 | `least` and `greatest` | 3,100 |
| `arrayLastIndex` | 6 | `if` | 889 |
| `tuple(...)` | 13 | `multiIf` | 100 |
| RAM, key-queue and lane reads | 7 | | |
| `arraySlice`, `arrayConcat`, `length` | 6 | | |
| `intDiv` and `modulo` | 5 | | |

Every `arrayElement` is a cut and there are about 3,900 of them. `intDiv`
and `modulo` are also non-compilable, but at 5 sites removing them for the
compiler's sake is not worth doing.

## Verdict

Making more of the fold compilable is rejected as a route to throughput. The
step expression already compiles as much as ClickHouse can compile of an
expression of this shape, and the compiled fraction is not where the time
is.

Almost everything scalar compiles. All integer casts fuse, and `multiIf`
fuses at 28 arms. The
non-compilable set is arrays, tuples, maps, `transform`, and integer
`intDiv` and `modulo`. `arrayElement` is non-compilable even on a literal
array, so this is not about captures. Arrays as a type are outside the
compiler's numeric-column model.

Amdahl's law is why the real fold gains nothing. A single register-file
rewrite per step drops the compiler's benefit from 5.78x to 4.30x, and the
real fold does that rewrite plus about 3,900 array reads, `arrayPushBack` on
three write-log arrays, an `arrayLastIndex` over the write log, and a
six-field `tuple` rebuild on every step. That work is the majority of the
runtime and the compiler cannot touch any of it.

The constructs that dominate are not removable by rewriting them into
compilable equivalents, because they are how the accumulator holds state.
The way to win is to do fewer of them, which is a data-structure change.
