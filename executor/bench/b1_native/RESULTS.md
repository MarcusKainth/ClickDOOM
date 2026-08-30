# B1 results: native ClickHouse against Docker Desktop

Harness and how to rerun: [README.md](README.md).

## Results

Arm C, native 26.3.25.2, 2026-08-28, boot window, K=60,000, HWM=20,000, three repeats of three chained batches:

| repeat | fold-alone instr/s | e2e instr/s |
|---|---|---|
| 1 | 1,042.6 | 1,020.0 |
| 2 | 1,043.9 | 1,024.4 |
| 3 | 1,039.5 | 1,065.3 |

Every batch retired 60,000 of 60,000. Per-batch fold time was 56.8 to 58.1 s.

The gameplay window did not produce a number. Its first batch retired 10,942 instructions and stopped on the write-log high-water mark.
The window's icount constant predates the #175 unroll and now lands in a store-dense stretch. The instrument refuses truncated batches by design.

Arm B, Docker 26.3.25.2, same day, same SQL text, same settings:

| repeat | fold-alone instr/s | e2e instr/s |
|---|---|---|
| 1 | 2,166.6 | 2,203.7 |
| 2 | 2,151.4 | 2,207.3 |
| 3 | 2,130.1 | 2,176.1 |

Per-batch fold time was 26.6 to 29.0 s. The gameplay window stopped on the high-water mark at the same point as arm C.

Docker is 2.07x faster than native for this fold on the same release. Arm A was not run: the owner judged the sprint's 26.3.17.4 figures sufficient, and B already answers the platform question.

### Why native is slower

Three microbenchmarks on the same two servers, `max_threads=1`, `compile_expressions=0`, best of three:

| test | native s | Docker s | Docker faster by |
|---|---|---|---|
| `sum(cityHash64(number))`, 3e8 rows, no allocation | 0.368 | 0.230 | 1.6x |
| arrayFold, 3e6 steps, scalar tuple only | 48.1 | 30.3 | 1.6x |
| arrayFold, 6e4 steps, register array rebuild + two arrayPushBack per step | 2.53 | 1.09 | 2.3x |

`system.build_options` differ in two ways that matter. The Linux build has `-flto=thin -fwhole-program-vtables --lto-whole-program-visibility`; the macOS build has no LTO.
The Linux build has `USE_EMBEDDED_COMPILER=ON`; the macOS build has it `OFF`, so `compile_expressions` cannot work there at all.

So about 1.6x is a flat build penalty on every function call, consistent with missing LTO on a codebase that dispatches one virtual call per expression node.
The allocation-heavy shape pays more on top, 2.3x, consistent with a slower allocator path on macOS.
The Docker VM itself is not the cost. Both compiler flags and `-march` are otherwise identical.

### jemalloc on macOS

ClickHouse/ClickHouse#108429: non-Linux builds compile jemalloc with `dirty_decay_ms:0`, so every `free` purges with an `madvise` syscall. Fixed upstream in 26.7.1. The `MALLOC_CONF` environment variable overrides it at runtime, so it was tested on the same 26.3.25.2 binary:

| `MALLOC_CONF` | alloc-heavy fold, 6e4 steps | scalar-only fold, 1e6 steps |
|---|---|---|
| default (`dirty_decay_ms:0`) | 2.81 s | 16.5 s |
| `dirty_decay_ms:5000` | 1.49 s | 15.4 s |
| `background_thread:true` | 3.27 s | 23.6 s |
| `dirty_decay_ms:5000,background_thread:true` | 1.91 s | 21.9 s |
| Docker 26.3.25.2, for reference | 1.12 s | 10.3 s |

`dirty_decay_ms:5000` alone removes most of the allocator penalty. `background_thread:true` makes things worse on this machine. The flat build penalty stays.

Real fold, native, `MALLOC_CONF=dirty_decay_ms:5000`, boot window, one repeat of three batches: fold-alone 1,770.7 instr/s, e2e 1,748.5 instr/s.
That is 1.7x better than native default and still 1.22x behind Docker on the same release.

### Differential trace on 26.3.25.2

`scripts/diff_run.sh 1048576` against a fresh 26.3.25.2 Docker container: 256 of 256 register checkpoints and 1 of 1 RAM and framebuffer checkpoints match refemu. No divergence. 846 instr/s at the clamped 4,096-instruction batch size.

### 26.7.5.10, the latest stable

Same instrument, boot window, one repeat of three batches each:

| arm | fold-alone instr/s | e2e instr/s |
|---|---|---|
| Docker 26.7.5.10 | 3,810.0 | 3,853.2 |
| native 26.7.5.10, default config | 3,596.2 | 3,711.0 |
| native 26.7.5.10, `MALLOC_CONF=dirty_decay_ms:5000` | 3,545.3 | 3,691.7 |

26.7 runs this fold 1.77x faster than 26.3.25.2 in Docker. On 26.7 the native build is within 6% of Docker, and the jemalloc override no longer matters.
Docker 26.7.5.10, three repeats of three batches: fold-alone 3,732.3 / 3,736.5 / 3,762.0 instr/s, e2e 3,843.3 / 3,815.0 / 3,851.7 instr/s.

`scripts/diff_run.sh 1048576` against a fresh 26.7.5.10 Docker container: 256 of 256 register checkpoints and 1 of 1 RAM and framebuffer checkpoints match refemu. No divergence. 1,474 instr/s at the clamped batch size, against 846 on 26.3.25.2.

Decisions: the run box stays on Docker. Native macOS ClickHouse is rejected for this workload. A pin bump from 26.3 to 26.7 is worth a `ci:` PR with the nightly deep-diff evidence SPEC section 8.3 requires.
