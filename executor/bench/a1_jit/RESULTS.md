# A1 results: what ClickHouse's expression JIT compiles in the arrayFold step

Harness and how to rerun: [README.md](README.md).

## Method

Ground truth is `system.query_log`'s ProfileEvents, never wall clock alone —
wall clock is how the `icount_base` cache-key bug hid for weeks.

    CompileFunction                 -> LLVM functions built = compiled ISLANDS
    CompileExpressionsMicroseconds  -> time spent in LLVM
    CompileExpressionsBytes         -> generated code size (8 KiB granular)

`min_count_to_compile_expression = 0` moves compilation to the **first**
execution, so one run per variant is decisive for the island counts; the
timing series (`run_series.sh`) uses the default `3` and >= 6 repeats so the
step change on the 4th run is visible as a series, not an average.

`gen.py` emits one `SELECT` per variant. Every probe is the same sandwich
inside an `arrayFold` lambda:

    sink = CHAIN(links/2, CONSTRUCT( CHAIN(links/2, toUInt64(acc.3)) ))

`CHAIN` is a nest of unambiguously compilable UInt64 arithmetic (4 function
nodes per link; no casts, no type change), so nothing can fragment it except
the construct under test. The construct takes the lower chain's value and the
upper chain continues from its result, so a construct LLVM cannot compile
necessarily **cuts** the chain. Read against `chain_only` (exactly 1 island):

| islands | meaning |
|---|---|
| 1 | the construct is compilable and fuses into the chain |
| 2 | the construct is NOT compilable — but it cuts only ITSELF out; the chain above and the chain below are each still compiled, with its result fed in as an input |
| 0 | the construct poisons the whole enclosing expression |

Determinism: the fixture is seeded entirely from `number`; no `now()`, no
`rand()` (SPEC §8.1). Timing is `query_duration_ms` from `system.query_log`
— measurement, not computation (PURITY.md). `max_ast_elements` /
`max_expanded_ast_elements` are raised to 4,000,000, and `max_ast_depth` /
`max_parser_depth` / `max_parser_backtracks` are raised **client-side** (a
trailing `SETTINGS` clause is parsed only after the body, so it cannot raise
a limit the body already tripped).

### Two traps this harness had to design around

1. **`CompileFunction` is a cache-MISS counter, not an island counter.**
   ClickHouse's compiled-expression cache is **server-global** and keyed by
   each island's **DAG** (not its SQL text). Rerunning an already-measured
   variant — or running a second variant that happens to build the same
   expression — reports `CompileFunction = 0`. `gen.py` therefore salts every
   variant's literals from `sha256(variant + A1_SALT_EPOCH)`; bump
   `A1_SALT_EPOCH` to rerun. This is also why `real_fold.sh` insists on a
   container whose cache is cold **for the real fold** — see Findings §1.
2. **A helper that references its argument twice explodes.** The first draft
   of the fragmentation helper was `plus(x, arrayElement(acc.2, ... x ...))`;
   at 23 insertions that is 2^23 copies and a 1.9 GB SQL file. Every
   construct in `gen.py` now references `x` at most linearly.

## Results (ClickHouse 26.3.17.4, `max_threads = 1`, private cold container)

`marg` = median minus the `floor` (near-empty step) median at the same K.
The sanity floor is `floor` in every table: `tuple(pc+4, acc.2, acc.3+1)`,
which at K = 100,000 costs 0.85-1.03 s and never changes with any setting.

### Pass 1 — which constructs are compilable (K = 100,000, 24 chain links)

`islands` = `CompileFunction` on a cold cache. 1 = the construct fuses into
the chain, 2 = it is not compilable and cuts the chain in two.

#### Compilable — fuse into the surrounding chain

| construct | islands | note |
|---|---|---|
| `plus` / `multiply` / `bitAnd` / `bitOr` / `bitXor` | 1 | the chain itself |
| `bitShiftLeft` / `bitShiftRight` | 1 | |
| `toUInt8` / `toUInt32` / `toUInt64` / `toInt32` / `toInt64` | 1 | **all integer casts compile** |
| `divide` (floating) | 1 | |
| `least` / `greatest` | 1 | |
| `abs` / `negate` | 1 | |
| comparisons (`less`, `equals`) | 1 | |
| `and` / `or` / `not` | 1 | |
| `if` | 1 | |
| `multiIf`, 4 arms | 1 | |
| `multiIf`, 12 arms | 1 | |
| `multiIf`, **28 arms** (9,064 AST nodes) | 1 | fuses whole; this is fold.py's `RESULT`/`NEXT` shape |

#### Not compilable — cut the chain

| construct | islands | note |
|---|---|---|
| `intDiv` | 2 | integer division traps on /0, so LLVM lowering is refused |
| `modulo` | 2 | same |
| `transform` | 2 | |
| `tuple(...)` construction | 2 | |
| `arrayElement` on the register array | 2 | |
| `arrayElement` on the `groupArray`-captured decode table | 2 | |
| `arrayElement` on a **literal** array | 2 | not a capture problem — arrays as such |
| `arrayLastIndex` with a lambda | 2 | |
| `arraySlice` + `length` | 2 | |
| `arrayConcat` | 2 | |
| `arrayPushBack` | 2 | |
| `map(...)[k]` | 2 | |

### Pass 1 — does a non-compilable node poison its enclosing subtree?

**No. It cuts only itself out.** Three placements, all at 24 chain links:

| variant | placement of the non-compilable node | islands | reading |
|---|---|---|---|
| `sib_arrayelem` | sibling: `plus(CHAIN, arrayElement(regs, 7))` | **1** | the parent `plus` compiles anyway, with the array read as an INPUT |
| `sib_intdiv` | sibling: `plus(CHAIN, intDiv(acc.3, 3))` | **1** | same |
| `sib_tupleelem` | sibling: `plus(CHAIN, acc.3)` | **1** | same |
| `sib_arraylength` | sibling: `plus(CHAIN, length(regs))` | **1** | same |
| `nc_bot` | at the leaf, under 24 links of chain | 2 | the whole chain above it still compiles |
| `nc_top` | at the root, above 24 links of chain | 2 | the chain below it still compiles |
| `arrayelem_regs` | mid-chain, in-path | 2 | chain above + chain below, both compiled |

A non-compilable node becomes an **input** to the compiled function around
it. It never disqualifies a parent, a sibling or an ancestor. This is the
decisive answer to the question the experiment was sent to settle: the
problem is **not** "one bad construct kills the whole lambda". It is
**fragmentation** — the expression is cut into many small islands.

### Pass 1 — fragmentation costs something, but not much

24 chain links total, with one non-compilable node inserted every `every`
links, so the island count is `24/every`. `jit0`/`jit1` are medians of 3
runs of the same query at `compile_expressions` 0 and 1.

| variant | islands | marg jit0 (ms) | marg jit1 (ms) | speedup |
|---|---|---|---|---|
| `frag_none` | 1 | 8865 | 2968 | **2.99x** |
| `frag_12` | 2 | 9371 | 3232 | 2.90x |
| `frag_8` | 3 | 9686 | 3385 | 2.86x |
| `frag_6` | 4 | 9649 | 3577 | 2.70x |
| `frag_4` | 6 | 10117 | 3950 | 2.56x |
| `frag_3` | 8 | 10618 | 4181 | 2.54x |
| `frag_2` | 12 | 11762 | 4707 | 2.50x |
| `frag_1` | 24 | 15650 | 6714 | **2.33x** |

Shredding a chain into 24 four-node islands costs 22% of the JIT's benefit
(2.99x -> 2.33x). It does not come close to eliminating it.

### Pass 1 — compile time vs island size (the calibration for Q3)

A single island of N chain links (~4 compilable function nodes each):

| links | ~fn nodes | AST nodes | compile us | compile bytes |
|---|---|---|---|---|
| 1 | 4 | 131 | 3,173 | 8,192 |
| 2 | 8 | 143 | 4,289 | 8,192 |
| 4 | 16 | 167 | 4,694 | 8,192 |
| 8 | 32 | 215 | 5,108 | 8,192 |
| 16 | 64 | 311 | 6,410 | 8,192 |
| 32 | 128 | 503 | 9,406 | 8,192 |
| 64 | 256 | 887 | 15,583 | 16,384 |
| 128 | 512 | 1,655 | 26,540 | 16,384 |

About **3.2 ms fixed per island plus ~46 us per compiled function node**.
(`chainlen_256` and above trip the server's 8 MiB analyzer stack — a
`TOO_DEEP_RECURSION` that is not a setting.)

### Pass 2 — the 4th-run step change (default `min_count_to_compile_expression = 3`)

Per-run series, K = 100,000, `compile_expressions = 1`, fresh salt so the
cache is cold. Reported as a series, not an average — the step IS the signal.

| run | `floor` ms | `frag_none` ms | islands | `frag_4` ms | islands |
|---|---|---|---|---|---|
| 1 | 855 | 9,856 | 0 | 10,761 | 0 |
| 2 | 945 | 10,272 | 0 | 10,847 | 0 |
| 3 | 965 | 9,859 | 0 | 11,107 | 0 |
| 4 | 918 | **3,880** | **1** (8,402 us) | **4,623** | **6** (17,384 us) |
| 5 | 888 | 3,917 | 0 | 4,933 | 0 |
| 6 | 936 | 3,842 | 0 | 4,524 | 0 |

`CompileFunction` fires on exactly the run where the wall clock steps, and
reads 0 on runs 5 and 6 **even though those runs are the fastest** — the
clearest possible demonstration that it counts cache misses, not islands.
The sanity floor is flat across all six runs and never compiles anything.

### Pass 2 — how much of the win is JIT-IMMUNE work? (`rw_*`)

`rw_N` adds N copies of fold.py's actual per-step register-file rewrite,
`arrayConcat(arraySlice(regs, 1, rd-1), [v], arraySlice(regs, rd+1))`, on top
of an unfragmented 24-link chain. `arrayConcat`/`arraySlice` are
non-compilable **and** allocate a fresh 31-element array every step, so this
prices work the JIT can never touch. Medians of 3, K = 100,000.

| variant | register rewrites per step | marg jit0 (ms) | marg jit1 (ms) | speedup |
|---|---|---|---|---|
| `rw_none` | 0 | 7,622 | 1,319 | **5.78x** |
| `rw_1` | 1 | 8,417 | 1,956 | 4.30x |
| `rw_2` | 2 | 9,163 | 2,546 | 3.60x |
| `rw_4` | 4 | 10,945 | 3,765 | **2.91x** |

**One** register-file rewrite per step — exactly what `fold.py`'s
`step_tuple` does once per instruction — costs a third of the JIT's benefit
(5.78x -> 4.30x). Four cost half. This is Amdahl, not a compiler limitation:
the rewrite is 10% of the interpreted time and 33% of the compiled time.

> Do not compare absolute speedups **across** families here. `rw_none` (5.78x)
> and `frag_none` (2.99x) are both unfragmented 24-link chains with identical
> AST node counts, and they reproducibly differ — measured back to back in one
> window: marg jit0 7,125 vs 9,211, marg jit1 1,312 vs 2,709. The only
> difference between them is that `frag_none` uses 24 distinct link constants
> while `chain_only`/`rw_none` reuse 12 twice, which plausibly changes how many
> 64-bit immediates LLVM has to materialise on aarch64. Not chased further —
> each family is internally consistent and run back to back, which is all the
> curves above require.

## Pass 3 — the REAL `executor/fold.py` step expression

`real_fold.sh` runs `python3 executor/fold.py K --db a1_jit_bench` — the
production generator, unmodified — on a private container.

### How much of it compiles

| | value |
|---|---|
| AST nodes (`EXPLAIN AST`, K = 2000) | **103,816** |
| lambda body | 324,444 bytes |
| `CompileFunction`, **cold** cache | **58** |
| `CompileExpressionsMicroseconds`, cold | **204,576** |
| `CompileExpressionsBytes`, cold | **499,712** |
| `CompileFunction`, warm cache (any later run in the same process) | 0 |

Against the calibration above (3.2 ms fixed + ~46 us per function node),
204,576 us over 58 islands is ~3.5 ms per island — at or below the cost of a
**four-function-node** island. So the expression is compiled into **58 very
small islands**, half a megabyte of machine code, and 205 ms of LLVM work.

The production reading this experiment was sent to explain —
`CompileFunction = 3`, `CompileExpressionsMicroseconds = 12652` — is a
**warm-cache** reading (55 hits, 3 misses), not a statement about how much of
the expression qualifies. See #166.

### What that compilation buys: nothing measurable

Paired runs, K = 100 (fixed cost only) and K = 100,100, interleaved
jit0/jit1 within each repeat so machine load is common-mode. n = 3.

| run | fixed j0 | fixed j1 | total j0 | total j1 | marg j0 | marg j1 | j0 - j1 |
|---|---|---|---|---|---|---|---|
| 1 | 26,133 | 25,278 | 82,521 | 82,295 | 56,388 | 57,017 | -629 |
| 2 | 24,632 | 24,824 | 84,904 | 81,881 | 60,272 | 57,057 | +3,215 |
| 3 | 25,173 | 25,540 | 85,995 | 86,435 | 60,822 | 60,895 | -73 |

| | `compile_expressions = 0` | `compile_expressions = 1` (warm) |
|---|---|---|
| marginal ms / 100,000 instructions | 59,161 (sd 2,417) | 58,323 (sd 2,228) |
| throughput | 1,690 instr/s | 1,715 instr/s |

**Paired difference 838 ms on ~59,000 ms = 1.4%; 95% CI -4,324 .. +5,999 ms,
i.e. speedup 1.014x with a 95% interval of roughly 0.93x .. 1.11x.** No
measurable effect. Any real JIT effect on the real fold larger than ~10% is
excluded by this data.

(Fixed per-batch cost at K = 100 is **25.3 s** against this 2 MiB fixture —
independent of the JIT, and much larger than anything measured here. That is
a separate problem.)

### Where the time actually goes

Counted over the lambda body of the emitted SQL (`realfold.sql`, 324,444
bytes). Textual site counts, not DAG nodes — the DAG common-subexpressions
these heavily — but they show the shape:

| non-compilable | sites | | compilable | sites |
|---|---|---|---|---|
| `DEC[...]` | 3,052 | | `toUInt32/64/8`, `toInt32/64` | 8,324 |
| `acc.2[...]` | 835 | | `bitAnd/Or/Xor/Shift*` | 3,331 |
| `arrayPushBack` | 10 | | `least`/`greatest` | 3,100 |
| `arrayLastIndex` | 6 | | `if` | 889 |
| `tuple(...)` | 13 | | `multiIf` | 100 |
| `RAMT[...]`/`KEYQT[...]`/`acc.3.N[...]` | 7 | | | |
| `arraySlice`/`arrayConcat`/`length` | 6 | | | |
| `intDiv`/`modulo` | 5 | | | |

Every `arrayElement` is a cut, and there are ~3,900 of them. `intDiv` and
`modulo` are also non-compilable, but there are only **5 sites** — removing
them is not worth doing for the JIT's sake.

## Findings

1. **The JIT compiles the real fold heavily, and it does not matter.** Cold,
   `fold.py`'s step compiles into 58 islands / 500 KB / 205 ms of LLVM. Warm,
   `compile_expressions = 1` is **1.014x** (95% CI 0.93x-1.11x) against
   `compile_expressions = 0` on the marginal per-instruction cost. The
   earlier framing — "only a fragment qualifies, so ~4.5x is still available"
   — is wrong in both halves.

2. **Almost everything scalar is compilable, including the constructs we
   assumed were not.** All integer casts (`toUInt32`/`toInt32`/`toUInt64`/
   `toInt64`/`toUInt8`) fuse. `multiIf` fuses at **28 arms** — fold.py's
   `RESULT` and `NEXT` are not blockers. `if`, comparisons, `and`/`or`/`not`,
   `least`/`greatest`, all bit ops, `abs`/`negate`, floating `divide`: all
   fuse.

3. **The non-compilable set is: arrays, tuples, maps, `transform`, and
   integer `intDiv`/`modulo`.** `arrayElement` is non-compilable even on a
   **literal** array, so this is not about `groupArray` captures — arrays as
   a type are simply outside the JIT's numeric-column model.

4. **A non-compilable node does NOT poison its enclosing subtree.** It
   becomes an *input* to the compiled function around it. A compilable parent
   with a non-compilable sibling argument still compiles (`sib_*`, all 1
   island); a chain above a non-compilable leaf still compiles; a chain below
   a non-compilable root still compiles. The failure mode is
   **fragmentation**, not poisoning.

5. **Fragmentation is a mild tax.** Shredding one island into 24 four-node
   islands costs 22% of the JIT's benefit (2.99x -> 2.33x), not 90%. Islands
   being small is not why the real fold gets nothing.

6. **Amdahl is why the real fold gets nothing.** Adding a single
   `arrayConcat`/`arraySlice` register-file rewrite per step — exactly what
   `step_tuple` does once per instruction — drops the JIT's benefit from
   5.78x to 4.30x; four drop it to 2.91x. The real fold does that rewrite
   plus ~3,900 array reads, `arrayPushBack` on three write-log arrays,
   `arrayLastIndex` over the write log, and a 6-field `tuple()` rebuild,
   every step. That work is the majority of the runtime and the JIT cannot
   touch any of it.

7. **`CompileFunction` is a cache-miss counter against a server-global,
   ActionsDAG-keyed cache.** Filed separately as **#166**. Every zero in an
   intermediate file from this experiment was a second execution, not an
   absence of compilation.

## Verdict

**REJECT** "make more of the fold compilable" (issue #167). There is no ~4.5x behind the
JIT. The expression already compiles as much as ClickHouse can compile of an
expression of this shape, and the compiled fraction is not where the time is.

The constructs that *could* be removed for compilability — `intDiv`, `modulo`
(5 sites) — are a rounding error. The constructs that dominate are
`arrayElement`, `arrayConcat`/`arraySlice`, `arrayPushBack` and `tuple`,
which are not removable by rewriting them into compilable equivalents: they
are how the accumulator holds state. The way to win is to **do fewer of
them** (B3/E3's decode index in the accumulator, B5/#123's register-file
rewrite), which is a data-structure change, not a JIT change — and which the
`rw_*` numbers above independently price.
