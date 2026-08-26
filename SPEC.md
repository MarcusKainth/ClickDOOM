# ClickDOOM SPEC

**SPEC_VERSION: 0.1.0** — ratified at the end of Phase 0 (see `docs/adr/`).

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
- Store into the text region (§2): fatal halt, reason = `SELF_MODIFY`, with pc
  and target address in the halt record. The executor pre-decodes text into a
  table (ADR-0002) because decoding inside the fold costs 7.4× the throughput;
  a write to text would silently invalidate that table. DOOM does not
  self-modify — if it appears to, that is a bug worth halting on.
- Reset state: `pc = 0x8000_0000`, all `x1..x31 = 0`. `crt0` sets `sp`, zeroes
  `.bss`, and jumps to `main`. `x0` hardwired to 0 (obviously — but the SQL
  register file must enforce writes to x0 being discarded).
- A fatal halt — any of §1's conditions above, or a `BAD_ADDR` access outside
  the memory map (§2) — does not retire the instruction that caused it:
  `icount` is not incremented, and no architectural state (`pc`, `rd`,
  memory) is modified. The halt record's `pc` identifies the faulting
  instruction. `icount` is load-bearing here, not cosmetic — §7's checkpoint
  trace and §3.1's elastic time both key on it. (#72 — ruled after sqlcpu's
  riscv-tests harness and refemu/executor disagreed on `icount` by exactly
  one on every fixture.)

## 2. Memory map

| Region       | Base         | Size          | Notes                                        |
|--------------|--------------|---------------|----------------------------------------------|
| RAM          | `0x8000_0000`| 24 MiB        | ROM image loaded at base; code+data+heap+stack |
| MMIO         | `0x1000_0000`| 4 KiB         | Registers, §3                                |
| FRAMEBUFFER  | `0x1100_0000`| 64,000 B      | 320×200, 8bpp palette-indexed, row-major     |
| PALETTE      | `0x1101_0000`| 768 B         | 256 × RGB (3 bytes), written on palette change |

Anything outside these regions: fatal halt, reason = `BAD_ADDR`.

Within RAM, the **text region** `[text_start, text_end)` is declared by the ROM
manifest (§4) and is **read-only**: it is the region the executor pre-decodes,
and a store into it is `SELF_MODIFY` (§1). The linker script places code and
nothing writable there.

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
  "size", "sha256", "text_start", "text_end"}`. The text bounds are absolute
  addresses delimiting the read-only region of §2; the build emits them from
  the linker script rather than having them written by hand.

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
- `decoded` — the pre-decoded text segment (ADR-0002), built by a SQL query over
  `ram` at ROM load and covering `[text_start, text_end)` only:
  `(word_addr UInt32, op_id UInt8, rd UInt8, rs1 UInt8, rs2 UInt8, imm UInt32,
  target UInt32, width_mask UInt32, sign_bit UInt32)`. `op_id` is the collapsed
  opcode space; `imm` is already sign-extended; `target` holds the absolute
  branch/jump target word index, or the link value for `jal`/`jalr`. Decoding
  must happen **inside** ClickHouse — decoding externally and inserting the
  result is a PURITY.md violation, not an optimization.

Materialize `ram` into the batch's constant array with `FINAL`, **not**
`argMax(value, version) ... GROUP BY word_addr`: measured 0.022–0.030 s against
0.245–0.256 s, and `FINAL` stayed flat with 1.2 M accumulated store deltas.

## 6. Batch execution contract

The driver invokes one batch = one `INSERT ... SELECT` executing up to `K`
instructions (`K` default **50,000**; tunable). 50,000 is the measured optimum,
not a guess: below it the ~0.30 s per-batch fixed cost dominates, above it the
write-log's superlinear growth cancels the remaining amortization (8,721 /
11,894 / 11,628 instructions/sec end-to-end at K = 10,000 / 50,000 / 200,000 —
`executor/bench/phase0/RESULTS.md`). A batch ends early on: halt,
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

## 9. Phase 0 resolutions and remaining open questions

Resolved by the Phase 0 benchmark (evidence:
`executor/bench/phase0/RESULTS.md`; decisions: ADR-0001, ADR-0002):

- [x] **arrayFold throughput.** 8,721 / 11,894 / 11,628 instructions per second
      end-to-end at K = 10,000 / 50,000 / 200,000, against ADR-0001's ≥10,000
      threshold. ADR-0001 **accepted**.
- [x] **Accumulator copy with large captured constant arrays.** Does not
      happen. Fold throughput is flat across a 6,144× range in captured-array
      size (4 KiB → 24 MiB: 113,895 vs 106,951 instructions/sec). Holding all
      24 MiB of RAM as a query-level constant is sound.
- [x] **Fallback decision.** Not needed — no fallback in ADR-0001's list was
      used. The decisive change was moving decode out of the fold lambda into a
      table (ADR-0002, 7.4× on the same fold), which the ADR did not anticipate
      because it assumed the cost model was about data movement rather than
      expression-node count.
- [x] **`K` default.** 50,000, fixed in §6 above.
- [x] **ClickHouse version pin.** 26.3 (tested against 26.3.17.4). Note the
      image restricts the `default` user to container-local addresses; the pin
      ships `CLICKHOUSE_PASSWORD` in `docker-compose.yml` and in every CI
      service container so host and runner connections work (see issue #3).

Deferred, with the milestone that closes them:

- [ ] **`IPMS` final value** (§3.1). Deliberately *not* derived from measured
      throughput — elastic time exists precisely so that emulator speed cannot
      affect emulated behaviour. It is a game-speed parameter: `IPMS` = 10,000
      means DOOM believes one millisecond passes per 10,000 retired
      instructions. Validated at ROM bring-up, when `refemu` can report how many
      instructions a real DOOM tic actually costs. Owner: `refemu`, Phase 1.
- [ ] **MMIO addresses and framebuffer format** (§2, §3). Cannot be ratified
      before the ROM boots; doomgeneric's platform layer may want a register
      this table does not have. Owner: `rom`, at the Phase 1 milestone.
