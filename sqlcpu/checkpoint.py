#!/usr/bin/env python3
"""SPEC §7 checkpoint trace emitter — sqlcpu workstream, issue #22.

The differential contract: refemu and sqlcpu must emit byte-identical trace
files. Every format decision here is refemu's (`refemu/src/refemu/trace.py`,
issues #15/#22) — this module reproduces it in SQL, not a second opinion on
it. Settled and cross-verified against a live ClickHouse instance before
this file existed (issue #15's comment thread has the worked examples);
`test_checkpoint.py` re-verifies the same examples here as committed,
runnable evidence rather than a one-off paste.

Decisions carried over verbatim from refemu's trace.py:
  * xxh64 seed 0 — matches ClickHouse's `xxHash64(x)` with no seed argument.
  * Hex fields: lowercase, zero-padded, no `0x` prefix. `pc_hex` is 8 digits,
    `reghash_hex`/`ramhash_hex`/`fbhash_hex` are 16. ClickHouse's `hex()` is
    uppercase and NOT zero-padded by default — `lpad(lower(hex(x)), N, '0')`
    is load-bearing, not decoration.
  * reghash: `pc || regs[1..31]`, each a 4-byte little-endian word,
    register-index order. x0 is never hashed (always 0 by construction).
  * ramhash: the full RAM region (SPEC §2, not MMIO/FRAMEBUFFER/PALETTE),
    address-ascending, each word little-endian.
  * fbhash (issue #55/#56): a separate column, not folded into ramhash —
    real diagnostic value in a divergence hunt (which region disagreed,
    without re-hashing to bisect). Bytes: FRAMEBUFFER (64,000 B) || PALETTE
    (768 B), address-ascending, that order. Present only alongside ramhash
    (same RAM_HASH_INTERVAL cadence).
  * Line format: `icount<TAB>pc_hex<TAB>reghash_hex[<TAB>ramhash_hex<TAB>fbhash_hex]`.

A real ClickHouse gotcha, found verifying this (see issue #15's comment):
`arrayStringConcat()` is NOT binary-safe over `FixedString`/`String` values
containing embedded null bytes — it silently truncates each element at the
first `\x00`, C-string-style. `reg_hash()` below uses plain `concat()`
instead (fixed small argument count, so this is fine); `word_array_hash()`
(RAM, arbitrary length) goes through hex text first, where there are no
null bytes to trip over, then one `unhex()` at the end.

This module builds SQL expression *text*, like decode.sql/execute.py's
composable functions — it does not read from any particular table.
Framebuffer/palette storage doesn't exist yet (Phase 2's "first
FRAME_COMMIT" milestone, executor's MMIO plumbing, SPEC §6) — `fb_hash()`
takes the current bytes as a caller-supplied expression rather than
assuming where they live, the same interface posture execute.py's
`alu_result()` takes for `loaded_word_expr`.
"""

# ---- hashing -----------------------------------------------------------

def reg_hash(pc="pc", regs="regs") -> str:
    """xxh64(pc || regs[1..31], each 4-byte LE) — refemu's reg_hash()."""
    words = [pc] + [f"{regs}[{i}]" for i in range(1, 32)]
    parts = ", ".join(f"reinterpretAsFixedString(toUInt32({w}))" for w in words)
    return f"xxHash64(concat({parts}))"


def bytes_hash(*byte_string_exprs: str) -> str:
    """xxh64 over one or more raw byte-string expressions, concatenated in
    the given order. For values already stored as `String`/`FixedString`
    (e.g. a committed framebuffer blob) — no word reinterpretation needed."""
    if len(byte_string_exprs) == 1:
        return f"xxHash64({byte_string_exprs[0]})"
    return f"xxHash64(concat({', '.join(byte_string_exprs)}))"


def word_array_hash(words_expr: str) -> str:
    """xxh64 over an Array(UInt32), each word little-endian, in array
    order — refemu's ram_hash() (caller supplies the array already
    address-ascending, e.g. `groupArray(value) FROM (... ORDER BY
    word_addr)` — see run_riscv_tests.py's DECODE_ARRAYS comment for why
    that capture needs the word_addr-paired-tuple form, not a bare
    per-column groupArray, to be trustworthy in the first place).

    Goes through hex text rather than `concat()`-ing raw FixedStrings:
    `arrayStringConcat` truncates at embedded nulls (this module's
    docstring), which a real RAM word can easily contain; hex digits never
    do, so it's mapped to hex text, safely concatenated, and unhex'd once.
    """
    return (
        f"xxHash64(unhex(arrayStringConcat(arrayMap("
        f"w -> hex(reinterpretAsFixedString(toUInt32(w))), {words_expr}"
        f"))))"
    )


def fb_hash(framebuffer="framebuffer", palette="palette") -> str:
    """xxh64 over FRAMEBUFFER || PALETTE (issue #55/#56), each supplied as
    a raw byte string already in address-ascending order. MMIO itself is
    deliberately excluded — live device state, not something two engines
    need to agree on bit-for-bit."""
    return bytes_hash(framebuffer, palette)


# ---- hex formatting ------------------------------------------------------

def hex64(expr: str) -> str:
    """16-digit lowercase zero-padded hex, for the 64-bit hash columns."""
    return f"lpad(lower(hex({expr})), 16, '0')"


def hex32(expr: str) -> str:
    """8-digit lowercase zero-padded hex, for `pc`."""
    return f"lpad(lower(hex({expr})), 8, '0')"


# ---- line formatting ------------------------------------------------------

def format_checkpoint(icount="icount", pc="pc", reghash="reghash",
                       ramhash=None, fbhash=None) -> str:
    """One SPEC §7 TSV line as a single SQL string expression:
    `icount<TAB>pc_hex<TAB>reghash_hex[<TAB>ramhash_hex<TAB>fbhash_hex]`.
    `ramhash`/`fbhash` are expression names (already-computed hash values,
    e.g. from reg_hash()/word_array_hash()/fb_hash() above bound via a
    caller's own WITH/arrayMap-let) — pass None for a plain-cadence line,
    both together for a RAM_HASH_INTERVAL line, matching refemu's
    format_checkpoint()'s "fbhash only ever alongside ramhash" contract.
    """
    fields = [f"toString({icount})", hex32(pc), hex64(reghash)]
    if ramhash is not None:
        fields.append(hex64(ramhash))
    if fbhash is not None:
        fields.append(hex64(fbhash))
    return "concat(" + ", '\t', ".join(fields) + ")"


def is_checkpoint(icount="icount", interval=4096) -> str:
    """True on a CHECKPOINT_INTERVAL boundary (default 4,096, SPEC §7)."""
    return f"({icount} % {interval} = 0)"


def is_ram_hash_checkpoint(icount="icount", interval=1_048_576) -> str:
    """True on a RAM_HASH_INTERVAL boundary (default 1,048,576, SPEC §7) --
    always also a checkpoint boundary, since RAM_HASH_INTERVAL is a
    multiple of CHECKPOINT_INTERVAL (256x, both defaults)."""
    return f"({icount} % {interval} = 0)"
