# Native ClickHouse against Docker Desktop

The run box executes the fold inside a Docker Desktop container on macOS. A
VM sits between the server and the hardware, so a native macOS build looks
like free throughput. This prices it, and the native build is slower.

## Question

Is native macOS ClickHouse faster than Docker Desktop for this fold, and if
not, why not? The same instrument then compares ClickHouse releases against
each other.

## Method

The canonical real-ROM throughput instrument drives every arm: the same SQL
text, the same settings, the boot window at K = 60,000 and
HWM = 20,000, three repeats of three chained batches, reporting fold-alone
and end-to-end instructions per second separately. Version arms are rotated
within the repeats so warm-up order cancels.

Three microbenchmarks then separate a build penalty from an allocator
penalty: a scalar aggregate with no allocation, an `arrayFold` over a scalar
tuple, and an `arrayFold` that rebuilds a register array and pushes to two
arrays per step.

A differential trace against the reference emulator runs on each candidate
server, checking register, RAM and framebuffer checkpoints.

## Conditions

| | |
|---|---|
| Date | 2026-08-28 to 2026-08-30 |
| Machine | Apple Silicon, macOS, 18 cores |
| K | 60,000 |
| HWM | 20,000 |
| Settings | `max_threads = 1`; `compile_expressions = 0` for the microbenchmarks |

The gameplay window produced no number on any arm. Its first batch retired
10,942 instructions and stopped on the write-log high-water mark, because
the window's icount constant lands in a store-dense stretch of the pinned
ROM. The instrument refuses to report throughput computed on a truncated
batch, since a batch that stops early measures different work than a full
one.

Three chained batches from reset are the slowest of a boot run. They are the
only ones that are both uncompiled, since `min_count_to_compile_expression`
defaults to 3, and holding a write log at the 20,000 high-water mark. A
14-batch chained series on the same server and settings measures batches 1 to 3
at 3,807.5 instructions per second end to end and batches 5 to 14 at 4,584.5,
recorded in [`compiled-node-cost.md`](compiled-node-cost.md). Every real-ROM
number below is a comparison between arms inside that first regime rather than
this fold's throughput.

## Results

### Native against Docker on the same release

Native 26.3.25.2, boot window, three repeats of three chained batches:

| repeat | fold-alone instr/sec | end-to-end instr/sec |
|---:|---:|---:|
| 1 | 1,042.6 | 1,020.0 |
| 2 | 1,043.9 | 1,024.4 |
| 3 | 1,039.5 | 1,065.3 |

Every batch retired 60,000 of 60,000. Per-batch fold time was 56.8 s to
58.1 s.

Docker 26.3.25.2, same day, same SQL text, same settings:

| repeat | fold-alone instr/sec | end-to-end instr/sec |
|---:|---:|---:|
| 1 | 2,166.6 | 2,203.7 |
| 2 | 2,151.4 | 2,207.3 |
| 3 | 2,130.1 | 2,176.1 |

Per-batch fold time was 26.6 s to 29.0 s. Docker is 2.07x faster than native
for this fold on the same release.

### Why native is slower

Three microbenchmarks on the same two servers, best of three:

| test | native s | Docker s | Docker faster by |
|---|---:|---:|---:|
| `sum(cityHash64(number))`, 3e8 rows, no allocation | 0.368 | 0.230 | 1.6x |
| `arrayFold`, 3e6 steps, scalar tuple only | 48.1 | 30.3 | 1.6x |
| `arrayFold`, 6e4 steps, register array rebuild and two `arrayPushBack` per step | 2.53 | 1.09 | 2.3x |

`system.build_options` differ in two ways that matter. The Linux build has
`-flto=thin -fwhole-program-vtables --lto-whole-program-visibility`; the
macOS build has no link-time optimisation. The Linux build has
`USE_EMBEDDED_COMPILER=ON`; the macOS build has it `OFF`, so
`compile_expressions` cannot work there at all.

About 1.6x is a flat build penalty on every function call, which is
consistent with missing link-time optimisation on a codebase that dispatches
one virtual call per expression node. The allocation-heavy shape pays 2.3x,
consistent with a slower allocator path on macOS. The Docker VM itself is
not the cost. Both compiler flags and `-march` are otherwise identical.

### The macOS allocator

Non-Linux ClickHouse builds compile jemalloc with `dirty_decay_ms:0`, so
every `free` purges with an `madvise` syscall. That is fixed upstream in
26.7.1 ([ClickHouse/ClickHouse#108429](https://github.com/ClickHouse/ClickHouse/issues/108429)).
The `MALLOC_CONF` environment variable overrides it at run time, so it was
tested on the same 26.3.25.2 binary:

| `MALLOC_CONF` | alloc-heavy fold, 6e4 steps | scalar-only fold, 1e6 steps |
|---|---:|---:|
| default (`dirty_decay_ms:0`) | 2.81 s | 16.5 s |
| `dirty_decay_ms:5000` | 1.49 s | 15.4 s |
| `background_thread:true` | 3.27 s | 23.6 s |
| `dirty_decay_ms:5000,background_thread:true` | 1.91 s | 21.9 s |
| Docker 26.3.25.2, for reference | 1.12 s | 10.3 s |

`dirty_decay_ms:5000` alone removes most of the allocator penalty.
`background_thread:true` makes things worse on this machine. The flat build
penalty stays. On the real fold, native with `MALLOC_CONF=dirty_decay_ms:5000`
over one repeat of three batches gives fold-alone 1,770.7 instr/sec and
end-to-end 1,748.5, which is 1.7x better than native default and still 1.22x
behind Docker on the same release.

### 26.7.5.10 against 26.3.25.2

Boot window, one repeat of three batches each:

| arm | fold-alone instr/sec | end-to-end instr/sec |
|---|---:|---:|
| Docker 26.7.5.10 | 3,810.0 | 3,853.2 |
| native 26.7.5.10, default config | 3,596.2 | 3,711.0 |
| native 26.7.5.10, `MALLOC_CONF=dirty_decay_ms:5000` | 3,545.3 | 3,691.7 |

26.7 runs this fold 1.77x faster than 26.3.25.2 in Docker. On 26.7 the
native build is within 6% of Docker and the jemalloc override makes no
difference. Docker 26.7.5.10 over three repeats of three batches gives
fold-alone 3,732.3, 3,736.5 and 3,762.0 instr/sec, and end-to-end 3,843.3,
3,815.0 and 3,851.7.

### 26.8.1.2041 against 26.7.5.10

Boot window, three repeats of three chained batches, arms rotated so arm A
runs first in repeats 1 and 3 and second in repeat 2.

Docker 26.7.5.10:

| repeat | fold-alone instr/sec | end-to-end instr/sec |
|---:|---:|---:|
| 1 | 3,857.8 | 3,829.9 |
| 2 | 3,644.3 | 3,814.1 |
| 3 | 3,787.4 | 3,839.3 |

Per-batch fold time was 14.9 s to 16.8 s.

Docker 26.8.1.2041, same day, same SQL text, same settings:

| repeat | fold-alone instr/sec | end-to-end instr/sec |
|---:|---:|---:|
| 1 | 2,835.9 | 2,774.3 |
| 2 | 2,988.3 | 2,822.1 |
| 3 | 3,082.4 | 3,029.1 |

Per-batch fold time was 19.2 s to 22.7 s. 26.8.1.2041 folds this boot window
1.27x slower than 26.7.5.10, mean fold-alone 2,968.9 against 3,763.2
instr/sec, and 1.33x slower end to end, 2,875.2 against 3,827.8. The gap
holds in both rotation orders, so it is not warm-up bias. Both images
resolved to native `linux/arm64` on this host, so it is not emulation
either.

### Differential traces

A differential run of 1,048,576 instructions against the reference emulator,
each on a fresh Docker container:

| server | checkpoints | instr/sec at the clamped 4,096-instruction batch size |
|---|---|---:|
| 26.3.25.2 | 256 of 256 register, 1 of 1 RAM and framebuffer | 846 |
| 26.7.5.10 | 256 of 256 register, 1 of 1 RAM and framebuffer | 1,474 |
| 26.8.1.2041 | 256 of 256 register, 1 of 1 RAM and framebuffer | 1,756 |

No divergence on any of the three. 26.8.1.2041 is faster at this batch size
despite the boot-window regression at K = 60,000. That is a single unrotated
sample at a different batch size, so it is something to look at rather than
a contradiction to resolve here.

## Verdict

The run box stays on Docker. Native macOS ClickHouse is rejected for this
workload: it is 2.07x slower on 26.3.25.2 and still slower on 26.7.5.10, and
its build cannot use `compile_expressions` at all.

Moving the pin from 26.3 to 26.7 is worth making, and needs the nightly
deep-diff evidence a version change requires.

26.8.1.2041 regresses the boot-window fold by roughly a quarter at
production batch size. That regression, rather than the differential trace,
is what should gate a further pin bump.
