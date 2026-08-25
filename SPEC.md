# ClickDOOM SPEC

**SPEC_VERSION: 0.1.0-draft** — DRAFT until ratified at the end of Phase 0.

This document is the single source of truth for every cross-workstream contract.
If code and SPEC disagree, the code is wrong. Changes require a `spec-change`
issue, a PR titled `spec: ...` (or `spec!: ...` if breaking), and approval from
the human owner (see CODEOWNERS). Breaking changes bump the minor version and
must update `SPEC_VERSION` here **and** the `spec_version` constants in code.

---

## 1. CPU contract

- ISA: **RV32IM**, little-endian. No FPU (DOOM is fixed-point). No compressed
  (C) extension — compile with `-march=rv32im -mabi=ilp32`.
- Privilege: machine-mode only, flat physical addressing, no MMU, no
  interrupts. All device interaction is polled MMIO (§3).
- `ecall`, `ebreak`, CSR instructions: **fatal halt** with reason code
  (should never appear in the ROM; if they do, the ROM is misbuilt).
- Misaligned word/halfword access: **fatal halt** (compile ROM with
  `-mstrict-align`). Keeps the SQL load/store path branch-free.
- Unimplemented/illegal opcode: fatal halt, reason = `ILLEGAL_INSN`, with pc
  and raw instruction word in the halt record.
- Reset state: `pc = 0x8000_0000`, all `x1..x31 = 0`. `crt0` sets `sp`, zeroes
  `.bss`, and jumps to `main`. `x0` hardwired to 0 (obviously — but the SQL
  register file must enforce writes to x0 being discarded).

## 2. Memory map

| Region       | Base         | Size          | Notes                                        |
|--------------|--------------|---------------|----------------------------------------------|
| RAM          | `0x8000_0000`| 24 MiB        | ROM image loaded at base; code+data+heap+stack |
| MMIO         | `0x1000_0000`| 4 KiB         | Registers, §3                                |
| FRAMEBUFFER  | `0x1100_0000`| 64,000 B      | 320×200, 8bpp palette-indexed, row-major     |
| PALETTE      | `0x1101_0000`| 768 B         | 256 × RGB (3 bytes), written on palette change |

Anything outside these regions: fatal halt, reason = `BAD_ADDR`.

Rationale for 8bpp + palette (not doomgeneric's default 32bpp buffer): 4×
fewer store instructions per frame on the emulated CPU. Frame conversion to
RGB happens in the render query (SQL side — allowed) at readout time.

## 3. MMIO registers (word access only)

| Addr          | Name          | R/W | Semantics                                                          |
|---------------|---------------|-----|--------------------------------------------------------------------|
| `0x1000_0000` | `TICKS_MS`    | R   | Emulated milliseconds = `instructions_retired / IPMS` (§3.1)       |
| `0x1000_0004` | `KEYQ`        | R   | Pop next key event; `0` if queue empty. Encoding: §3.2             |
| `0x1000_0008` | `EXIT`        | W   | Halt emulation; written value = exit code                          |
| `0x1000_000C` | `PUTCHAR`     | W   | Debug console: append low byte to `console_out` table              |
| `0x1000_0010` | `FRAME_COMMIT`| W   | ROM signals framebuffer complete; value = frame number             |

### 3.1 Elastic time
`IPMS` (instructions-per-emulated-millisecond) is a constant in
`executor/config`, default **10,000** (≈10 MHz virtual CPU). Time advances
with retired instructions, never wall clock. This makes execution fully
deterministic and speed-independent: `-timedemo` produces identical frames
whether the emulator runs at 1 kIPS or 1 MIPS. **Never** derive time from
`now()` or batch wall time.

### 3.2 Key event encoding
`KEYQ` read returns `(pressed << 8) | doomkey` where `pressed ∈ {0,1}` and
`doomkey` is the doomgeneric keycode. Reads pop exactly one event; the queue
is a ClickHouse table (`input_queue`) populated by the driver, ordered by
`event_seq`. A read when empty returns 0 and pops nothing.

## 4. ROM artifact format

Deliverable from `rom/`: two files, built reproducibly (pinned toolchain
image, no timestamps):

- `doom-rv32im.bin` — flat binary, loaded verbatim at `0x8000_0000`.
  Contains code, rodata, the embedded shareware `doom1.wad`, and zero-init
  markers. **Note:** shareware `doom1.wad` is freely redistributable; the
  DOOM source is GPL. Do not embed commercial WADs in the repo.
- `manifest.json` — `{"spec_version", "entry": 2147483648, "load_addr",
  "size", "sha256"}`.

CI pins the expected `sha256` in `rom/PINNED_HASH`; a mismatch on an
unrelated PR means the build went nondeterministic — treat as `P0`.

## 5. Emulator state schema (ClickHouse)

Authoritative DDL lives in `sqlcpu/schema.sql`; this section defines the
shape. All tables carry `spec_version String`.

- `cpu_state` — one row per committed batch: `(batch_id UInt64, icount
  UInt64, pc UInt32, regs Array(UInt32) /* len 31, x1..x31 */, halted UInt8,
  halt_reason LowCardinality(String), exit_code UInt32)`.
- `ram` — `ReplacingMergeTree(version)` keyed by `word_addr UInt32` (byte
  addr >> 2), `value UInt32`, `version UInt64` (= icount of the store).
  Loaded once per batch as a constant array; stores inside a batch live in
  the fold's write-log and are flushed here on batch commit.
- `input_queue` — `(event_seq UInt64, key_event UInt16, consumed UInt8)`.
- `frames_out` — `(frame_no UInt32, committed_icount UInt64, fb String /*
  64,000 bytes */, palette String /* 768 bytes */)`; written by the render
  query on `FRAME_COMMIT`.
- `console_out` — `(seq UInt64, byte UInt8)`.

## 6. Batch execution contract

The driver invokes one batch = one `INSERT ... SELECT` executing up to `K`
instructions (`K` default 50,000; tunable). A batch ends early on: halt,
`FRAME_COMMIT` write, or write-log high-water mark. Batch commit is atomic:
either all effects (ram deltas, cpu_state row, MMIO side effects) land or
none do. The driver's only logic: loop batches, insert key events, blit
committed frames. See PURITY.md.

## 7. Differential trace & checkpoint format

Both `refemu` and `sqlcpu` must emit identical checkpoints:

- Every `CHECKPOINT_INTERVAL` (default 4,096) retired instructions:
  `(icount, pc, xxh64(pc || regs[1..31] as LE bytes))`.
- Every `RAM_HASH_INTERVAL` (default 1,048,576) instructions: additionally
  `xxh64` over the full RAM region as LE words.
- Trace file: one checkpoint per line, TSV:
  `icount<TAB>pc_hex<TAB>reghash_hex[<TAB>ramhash_hex]`.
- First divergence = first line that differs. `just diff N` runs both
  engines N instructions and reports it; divergences are filed with the
  `divergence-report` issue form.

## 8. Determinism rules (all workstreams)

1. No wall-clock, no randomness, no host environment reads on any
   computation path.
2. Iteration order that affects results must be explicit (`ORDER BY`
   everywhere it matters; never rely on ClickHouse block order).
3. ClickHouse server version is pinned repo-wide (see `docker-compose.yml`
   and workflow files — keep in sync). Bumps are `ci:` PRs with full
   nightly deep-diff evidence.
4. The ROM build must be byte-reproducible (§4).

## 9. Open questions (Phase 0 must resolve)

- [ ] arrayFold throughput benchmark: instructions/sec at K=10k/50k/200k;
      accumulator copy behavior with large captured constant arrays.
- [ ] Fallback decision recorded as ADR if arrayFold underperforms
      (recursive CTE? smaller K? paged accumulator?).
- [ ] Final `IPMS` and `K` defaults from measured throughput.
- [ ] Ratify MMIO addresses & framebuffer format after ROM bring-up in QEMU.
- [ ] ClickHouse version final pin.
