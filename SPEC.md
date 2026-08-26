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
- `ecall`: fatal halt, reason = `ECALL`. `ebreak`: fatal halt, reason =
  `EBREAK`. Any CSR instruction (`csrrw`/`csrrs`/`csrrc`/`csrrwi`/`csrrsi`/
  `csrrci`): fatal halt, reason = `CSR`. None should ever appear in the ROM;
  if they do, the ROM is misbuilt.
- `fence`/`fence.i`: **not** a fatal halt — a plain retiring no-op (decodes
  onto the same collapsed arm as a real no-op, e.g. `addi x0,x0,0`; `pc +=
  4`, no other state change). A single hart with no cache has nothing to
  reorder against, and the toolchain emits these from compiled C — halting on
  them would break the ROM. Agreed cross-engine in issue #37; no automated
  test currently exercises this (riscv-tests' `fence_i.S` is excluded, since
  it also exercises self-modifying code, which ADR-0002 forbids outright —
  see #37), so treat "does my decode give FENCE its own arm, or does it fall
  through to `ILLEGAL_INSN`?" as a thing to verify by reading the decode
  path, not by memory.
- Misaligned word/halfword **data** access (load/store): fatal halt, reason =
  `MISALIGNED`, with pc and target address in the halt record (compile ROM
  with `-mstrict-align`, keeping the SQL load/store path branch-free).
- Misaligned **jump/branch target** (`jal`/`jalr`/a taken branch computes a
  target not aligned to 4 bytes): fatal halt, reason = `MISALIGNED`, checked
  **at the transferring instruction, eagerly** — matching real RISC-V's
  instruction-address-misaligned semantics, where the exception is reported
  on the branch/jump, not on the target. The halt record's pc is the
  transferring instruction's own pc (not the unreached target), and neither
  `pc` nor `rd` is updated — the jump does not architecturally complete.
  Ruled in issue #37 over the alternative (deferring the check to the next
  fetch, with the target as the halt's pc): the eager check keeps the halt
  inside the instruction that caused it, and removes an ordering dependency
  between engines over exactly when "the next fetch" happens relative to
  batch boundaries. In practice unreachable from a correctly-built ROM (no
  compressed-instruction extension means every real target is 4-byte
  aligned, and `jalr` clears bit 0) — which is exactly why it needs pinning
  rather than being left to each engine's judgment: an unreachable path never
  gets exercised by a passing test, so the written agreement is the only
  thing keeping two independent engines aligned on it.
- Unimplemented/illegal opcode: fatal halt, reason = `ILLEGAL_INSN`, with pc
  and raw instruction word in the halt record.
- Store into the text region (§2): fatal halt, reason = `SELF_MODIFY`, with pc
  and target address in the halt record. The executor pre-decodes text into a
  table (ADR-0002) because decoding inside the fold costs 7.4× the throughput;
  a write to text would silently invalidate that table. DOOM does not
  self-modify — if it appears to, that is a bug worth halting on.
- Halt-reason vocabulary is closed and exact-match: `ILLEGAL_INSN`,
  `BAD_ADDR`, `SELF_MODIFY`, `MISALIGNED`, `ECALL`, `EBREAK`, `CSR`, `EXIT` —
  all uppercase ASCII, no punctuation, no per-engine normalization. It is
  observable state (`cpu_state.halt_reason`, §5) that both `refemu` and
  `sqlcpu` must produce identically; agreed cross-engine in issue #37.
  The first seven are **faults**; `EXIT` is not. It is the ROM's own clean
  stop, written to §3's `EXIT` register, and it appears here because §5's
  `cpu_state` routes every way the machine can stop through this one column.
  "Halted normally" is therefore `halt_reason = 'EXIT'`, with the written
  value in `exit_code` — not an empty string, which would be
  indistinguishable from an unset column in a differential comparison.
  A fault never sets `exit_code`, and `EXIT` never sets a fault reason, so
  the two are always separable. This matters at exactly one moment and it is
  an expensive one: `EXIT` is how `-timedemo demo3` terminates, so it is the
  **last** value a victory run produces, and a cross-engine mismatch here
  fails the §7 final-state comparison after everything else has already
  agreed. No riscv-test writes that register.
- Reset state: `pc = 0x8000_0000`, all `x1..x31 = 0`. `crt0` sets `sp`, zeroes
  `.bss`, and jumps to `main`. `x0` hardwired to 0 (obviously — but the SQL
  register file must enforce writes to x0 being discarded).
- A fatal halt does not retire the instruction that caused it: `icount` is
  not incremented, and no architectural state (`pc`, `rd`, memory) is
  modified. The halt record's `pc` identifies the faulting instruction.
  `icount` is load-bearing here, not cosmetic — §7's checkpoint trace and
  §3.1's elastic time both key on it. This applies to every fatal halt
  regardless of which section defines it — including `BAD_ADDR` (§2), not
  only §1's list above. (#72 — ruled after sqlcpu's riscv-tests harness and
  refemu/executor disagreed on `icount` by exactly one on every fixture.)

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

Accesses to the MMIO window that are not word-width, or whose address is
not one of the five register offsets above, **read as 0 and are silently
ignored on write** — no side effect, no fatal halt. Reproducing a
byte-addressable scratch region for this window would cost node-evaluation
budget in the executor's fold on every retired instruction (§6) to serve
behavior no ROM address exercises; DOOM's platform layer only ever declares
these five offsets as `volatile uint32_t *`. Agreed cross-engine in issue
#87.

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
  halt_reason LowCardinality(String), exit_code UInt32)`. A **durable table,
  never pruned**, derived from `batch_commit` (below) by the same idempotent
  flush that populates `ram` and `console_out` — a fourth derivation on
  identical terms, not a special case. `ReplacingMergeTree` keyed by
  `batch_id`, so a flush redone after a crash cannot leave two rows for one
  batch; the derivation is deterministic from a single `batch_commit` row, so
  any duplicates would be byte-identical anyway, but "one row per committed
  batch" should be literally true of the table rather than merely true of
  what a reader happens to select. Read it with `FINAL` when the row *count*
  or the full history matters; the per-batch state reload (`ORDER BY
  batch_id DESC LIMIT 1`) does not need it, since a duplicate pair is
  content-identical and either row answers correctly.
- `batch_commit` — the batch's single atomic write (§6). One row per batch,
  superset of `cpu_state`'s columns plus the per-batch bulky data recovery
  needs to safely re-derive `ram` and `console_out`: `keyq_pos UInt64`
  (cumulative KEYQ pops through this batch), `has_frame UInt8` / `frame_no
  UInt32` (FRAME_COMMIT, if any, this batch), `wl_addr Array(UInt32)`,
  `wl_val Array(UInt32)`, `wl_icount Array(UInt64)` (per-store version — see
  the versioning note below), `console_bytes Array(UInt8)`. Bounded by
  retention on **batch_id lag, not wall-clock time**: only the most recent N
  rows are kept (N = 16, `executor/config`), older ones dropped **whole** by
  a fixed statement (partition-drop, or a delete keyed on `batch_id <
  (SELECT max(batch_id) - N FROM batch_commit)`) the driver issues
  unconditionally every batch. Retention drops entire rows rather than
  individual columns, and touches **only** `batch_commit` — `cpu_state` is
  derived and kept forever, so §5's "one row per committed batch" survives
  the window. That asymmetry is the point: the atomic write is short-lived
  scaffolding, the derived state is permanent — the threshold is computed inside the query, not decided by
  driver logic, so this stays within PURITY.md's housekeeping allowance. In
  normal operation only the latest row's bulky columns are ever read
  (everything older has already been flushed into `ram`/`console_out`), so
  N=16 is generous headroom, not a tight bound.
  **Rejected alternative: a wall-clock TTL** (an earlier draft of this PR
  used `commit_ts DateTime DEFAULT now()` with a 1-day column TTL).
  Rejected because it reintroduces exactly the failure mode this table
  exists to avoid: a driver outage longer than the TTL window loses the
  last committed batch's write-log before recovery can replay it, silently
  and permanently diverging `ram` from `cpu_state` with nothing to
  reconcile from. For this project that is not an edge case — a `demo3`
  timelapse run is multi-week at realistic throughput, machines sleep, and
  work routinely pauses on a human gate, so "the outage exceeds one day" is
  an expected occurrence, not an exotic one. Batch-id-lag retention is
  strictly better on every axis that mattered: no clock anywhere (the
  `now()` and its purity annotation disappear entirely), no outage hazard
  (recovery is unconditional, independent of how long the driver was down),
  deterministic per §8, and a tighter bound in practice than a day's worth
  of batches.
- `ram` — `ReplacingMergeTree(version)` keyed by `word_addr UInt32` (byte
  addr >> 2), `value UInt32`, `version UInt64` (= icount of the store).
  Loaded once per batch as a constant array; stores inside a batch live in
  the fold's write-log and are flushed here on batch commit. The version for
  each delta must be the individual store's own `icount`
  (`batch_commit.wl_icount[i]`), not the batch's final `icount` — two
  same-address stores in one batch sharing a version is a
  `ReplacingMergeTree` tie with an unspecified winner, which violates §8's
  explicit-ordering rule.
- `input_queue` — `(event_seq UInt64, key_event UInt16, consumed UInt8)`.
- `frames_out` — `(frame_no UInt32, committed_icount UInt64, fb String /*
  64,000 bytes */, palette String /* 768 bytes */)`; written by the render
  query on `FRAME_COMMIT`.
- `console_out` — `(seq UInt64, byte UInt8)`.
- `decoded` — the pre-decoded text segment (ADR-0002), built by a SQL query over
  `ram` at ROM load and covering `[text_start, text_end)` only:
  `(word_addr UInt32, id UInt8, rd UInt8, rs1 UInt8, rs2 UInt8, imm UInt32,
  tgt UInt32, mk UInt32, sg UInt8, raw UInt32)` — names and types match
  `sqlcpu/schema.sql` literally, as every other table in this section does.
  `id` is the collapsed opcode space, including dedicated arms for the fatal-halt
  decode cases (§1): `ecall`, `ebreak`, CSR, and unimplemented/illegal each
  get their own `id`, disjoint from the executable arms. `imm` is already
  sign-extended; `tgt` holds **only** the absolute branch/jump target as a
  **byte address** — not a word index — (a word index discards bit 1, which
  is exactly the bit misaligned-target detection needs; an earlier draft was
  word-indexed and was reverted for that reason, see §1's eager alignment
  check) for branches and `jal` (not `jalr`, which is register-relative and
  computed live) — it is never the link value. The link value `jal`/`jalr`
  write to `rd` (`pc + 4`) is **not** stored in `decoded`; it is computed live
  from the executing pc, since it is simple pc-relative arithmetic with
  nothing to gain from precomputing. An earlier draft of this table used
  `target` for both the jump target and the link value on the same row —
  caught in review (issue discussion on PR #42/#48) before it shipped as a
  real bug: harmless in the Phase 0 benchmark this design is descended from,
  since that benchmark's decode data is synthetic and never executed
  (`executor/bench/phase0/RESULTS.md` §6), but a genuine correctness bug in a
  table meant to produce correct results. `raw` carries the original
  instruction word, needed only for the `ILLEGAL_INSN` halt record (§1) since
  every other column replaces the raw word rather than preserving it.
  Decoding must happen **inside** ClickHouse — decoding externally and
  inserting the result is a PURITY.md violation, not an optimization.

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
none do. "Atomic" is a statement about externally observable state, not a
requirement for a single cross-table transaction — ClickHouse has none. A
design satisfies this contract if the batch's committed facts are captured
in one atomic single-row write (`batch_commit`, §5), and every other effect
converges to match that row through derivation that is safe to redo any
number of times, such that no observer — including a SPEC §7 differential
trace, before or after a crash and restart — can ever witness a
half-applied batch. The driver's only logic: loop batches, insert key
events, blit committed frames. See PURITY.md.

## 7. Differential trace & checkpoint format

Both `refemu` and `sqlcpu` must emit identical checkpoints:

- Every `CHECKPOINT_INTERVAL` (default 4,096) retired instructions:
  `(icount, pc, xxh64(pc || regs[1..31] as LE bytes))`.
- Every `RAM_HASH_INTERVAL` (default 1,048,576) instructions: additionally
  `xxh64` over the full RAM region as LE words, **and** a second, independent
  `xxh64` over FRAMEBUFFER (64,000 B) concatenated with PALETTE (768 B), both
  in address-ascending order (64,768 bytes total) — `fbhash`. MMIO is
  excluded from both hashes: it is live device state, not a value two
  independently-running engines are expected to agree on bit-for-bit.
  `fbhash` exists because a store that lands the wrong value at the right
  framebuffer/palette address (or vice versa) touches neither a register nor
  RAM, so `reghash`/`ramhash` alone are blind to exactly the class of bug
  most likely to matter for DOOM specifically — a rendering bug that
  wouldn't surface until the final frame comparison, with no checkpoint in
  between narrowing down where it happened. A separate column (not folded
  into `ramhash`) trades a larger trace-format surface for telling a
  divergence hunt *which* region diverged without bisecting — real
  diagnostic value specifically in the Phase 3 desync hunt this format
  exists for. Agreed by `refemu` and `sqlcpu` independently (issue #55).
- Trace file: one checkpoint per line, TSV:
  `icount<TAB>pc_hex<TAB>reghash_hex[<TAB>ramhash_hex<TAB>fbhash_hex]`.
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
