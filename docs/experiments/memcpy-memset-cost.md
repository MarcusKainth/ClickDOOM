# memcpy and memset in the ROM

An external reviewer hypothesised that DOOM's renderer is memcpy-saturated
and that the ROM's syscall shim provides naive byte-loop `memcpy` and
`memset` implementations costing 4 to 6 emulated instructions per byte,
where a word-wise implementation costs about 1.5. If that were true, fixing
the shim would cut the total instruction count of a run, which is a
different lever from making each instruction cheaper.

## Question

Are `memcpy` and `memset` byte-loop shims, and what do they cost in
instructions per byte?

## Method

The ROM runs under the reference emulator and every retired program counter,
not a sample, is attributed to an ELF `STT_FUNC` symbol, so the output is an
exact per-function instruction budget.

On top of that, the entry program counter of `memcpy`, `memset`, `memmove`,
`memcmp`, `strlen`, `strcpy`, `strcmp` and `strncpy` is trapped and the
ilp32 argument registers read. That gives per-function call counts, total
bytes requested, a power-of-two length histogram, and for the copy routines
the fraction of calls whose operands are mutually word-aligned,
`(src ^ dst) & 3 == 0`, which is the condition newlib's fast path requires.
Dividing instructions by bytes gives instructions per byte directly, with no
modelling.

Four windows are reported: the whole run, boot from 0 to the first frame
commit, steady state from the first to the last frame commit, and named
frame ranges.

Boot has to be read separately. It contains the C runtime's `.bss`-zeroing
loop, `_start` at 184,048 instructions of stores and no loads, and the
one-shot WAD load of about 3.8 MB, both of which skew any whole-run
histogram.

Nothing on a result path reads a host clock or any randomness, so a run is a
pure function of the ROM image, the manifest and the frame count.

## Conditions

| | |
|---|---|
| Date | 2026-08-26 |
| ROM | `eabb12ed…b92c`, the pinned hash on that date |
| Run | 300 frames, 392,488,490 instructions |
| Emulator | the reference emulator, about 170M instructions/sec |
| Steady-state window | frames 200 to 299, 158,555,736 instructions |

The steady-state window is the one that matters: frames 0 to about 150 are
title screen and menu, where `V_DrawPatch` dominates and the two rasterizers
are absent.

## Results

The ROM's syscall shim does not define `memcpy`, `memset`, `memmove` or any
string routine. It only calls two of them. The ROM links the pinned
toolchain's newlib, so `memcpy` and `memset` are newlib's, and both
disassembly and the attribution profile show they are already word-wise.

### Steady state, frames 200 to 299

| routine | instructions | share | calls | bytes | instr/byte | word-aligned calls |
|---|---:|---:|---:|---:|---:|---:|
| `memcpy` | 5,590,460 | 3.53% | 30,486 | 6,683,176 | 0.836 | 98.6% |
| `memset` | 307,917 | 0.19% | 1,978 | 616,024 | 0.500 | 16.3% |

Top functions in the same window:

| function | share | instructions |
|---|---:|---:|
| `R_DrawSpan` | 34.68% | 54,981,710 |
| `R_DrawColumn` | 22.09% | 35,018,425 |
| `R_RenderSegLoop` | 6.65% | 10,543,098 |
| `R_DrawFuzzColumn` | 4.55% | 7,217,492 |
| `R_MakeSpans` | 3.80% | 6,020,496 |
| `memcpy` | 3.53% | 5,590,460 |
| `R_DrawMaskedColumn` | 3.13% | 4,960,603 |
| `DG_DrawFrame` | 2.39% | 3,783,567 |

### Whole run

| routine | instructions | share | calls | bytes | instr/byte | word-aligned calls |
|---|---:|---:|---:|---:|---:|---:|
| `memcpy` | 19,390,841 | 4.94% | 93,105 | 24,384,676 | 0.795 | 98.6% |
| `memset` | 814,587 | 0.21% | 5,781 | 1,664,344 | 0.489 | 41.9% |
| `strcmp` | 406,412 | 0.10% | 14,292 | | | 100.0% |
| `strncpy` | 146,802 | 0.04% | 2,886 | 23,127 | 6.348 | 99.9% |
| `strlen` | 94,308 | 0.02% | 3,165 | | | 99.9% |
| `memmove` | 17,690 | 0.00% | 547 | 3,033 | 5.833 | 55.2% |

The two routines with high instructions per byte, `memmove` at 5.833 and
`strncpy` at 6.348, move 3,033 and 23,127 bytes across the entire run.

### Boot, 0 to the first frame commit

15,653,137 instructions, of which `memcpy` is 2,831,723 (18.09%),
`R_InitTextureMapping` 2,745,991 (17.54%), `strncasecmp` 2,295,116 (14.66%),
`_start` 184,048 (1.18%) and `memset` 171,792 (1.10%). The `memcpy` share
here is the one-shot WAD load and does not describe gameplay.

### Call sizes, whole run, power-of-two buckets

```
memcpy:  0:3  2:3  4:16  8:12,866  16:582  32:13,716  64:1,062
         128:271  256:60,778  512:675  1024:3,128  32768:5
memset:  8:1,045  16:550  32:16  64:59  128:336  256:3,367  512:399
         1024:1  2048:2  4096:1  8192:1  16384:1  32768:3
memmove: 1:239  2:75  4:146  8:41  16:30  32:12  64:4
```

### Reproduction against a second ROM

The same parameters were run against an earlier build, `22113f55…c955ed`,
which boots to the attract-mode loop with no fixed timedemo argument. In the
same frame window it gives `memcpy` at 0.838 instructions per byte and 3.81%
of instructions, against 0.836 and 3.53% for the pinned build. The
conclusion reproduces closely, because the copy routine is the same in both.

`DG_DrawFrame`'s share is 5.41% on the attract-mode build and 2.39% on the
pinned build. A separately measured 8x unroll of that function reduced its
own cost by about 53%, and 2.39/5.41 is about 0.44, so about 56% here,
consistent within the noise of comparing two windows that do not hold the
same content.

What does not reproduce is the rank of the two rasterizers. The attract-mode
build gives `R_DrawColumn` 30.86% and `R_DrawSpan` 22.35%; the pinned build
gives `R_DrawSpan` 34.68% and `R_DrawColumn` 22.09%. This is neither
measurement noise nor a harness fault. The two runs profile two different
programs, an attract-mode loop cycling demos and title screen against a
specific recorded demo, and a wall rasterizer against a span rasterizer
ratio depends on the wall-to-floor proportions of whichever rooms are on
screen. Any claim about the split between those two functions has to name
the ROM it was measured on.

## Verdict

The hypothesis is rejected. `memcpy` and `memset` are newlib's, already
word-wise, and cost 0.836 and 0.500 instructions per byte in the
steady-state window, well under the 1.5 a word-wise implementation was
expected to cost and far under the 4 to 6 the hypothesis assumed. 98.6% of
`memcpy` calls are mutually word-aligned, so the fast path is taken.

`memcpy` is 3.53% of steady-state instructions, so removing it entirely
would not be a lever. There is nothing to fix in the memory routines.
