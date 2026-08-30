//! The batch fold: SPEC halt semantics, early termination (halt, write-log
//! high-water mark, FRAME_COMMIT), and the write-log versioning fix (every
//! write-log entry carries its own retiring instruction's icount, not the
//! batch's final icount).

use clickdoom_spec::{
    FRAMEBUFFER_BASE, FRAMEBUFFER_SIZE, MMIO_BASE, MMIO_SIZE, PALETTE_BASE, PALETTE_SIZE, RAM_BASE,
};

use crate::config::{
    HALT_BAD_ADDR, HALT_CSR, HALT_EBREAK, HALT_ECALL, HALT_EXIT, HALT_ILLEGAL_INSN,
    HALT_MISALIGNED, HALT_NONE, HALT_REASON_NAMES, HALT_SELF_MODIFY, OP_CSR, OP_EBREAK, OP_ECALL,
    OP_ILLEGAL, OP_LOAD, OP_STORE,
};

/// `(ADDR, bad_addr_cond, misaligned_cond, WA_safe)`. `wa_safe` is masked
/// into `[0, ram_words)` unconditionally, so `arrayElement` on RAM/write-log
/// arrays never throws regardless of whether the access is actually in
/// range; `bad_addr_cond` is the real (unmasked) bounds check used for halt
/// detection, computed independently.
struct AddrAndAlign {
    addr: String,
    bad_addr_cond: String,
    misaligned_cond: String,
    wa_safe: String,
}

fn addr_and_align(a: &str, imm: &str, dmkv: &str, ram_base: u32, ram_words: u32) -> AddrAndAlign {
    let addr = format!("toUInt32({a} + {imm})");
    let addr64 = format!("toUInt64({addr})");
    let ram_end = ram_base + ram_words * 4;
    let bad_addr_cond = format!("({addr64} < {ram_base} OR {addr64} >= {ram_end})");
    let align_mask = format!("multiIf({dmkv}=4294967295, 3, {dmkv}=65535, 1, 0)");
    let misaligned_cond = format!("(bitAnd({addr}, {align_mask}) != 0)");
    let wa_safe = format!(
        "least(bitShiftRight(toUInt32(toUInt64({addr}) - {ram_base}), 2), {})",
        ram_words - 1
    );
    AddrAndAlign {
        addr,
        bad_addr_cond,
        misaligned_cond,
        wa_safe,
    }
}

/// Proves (not assumes) that `addr_and_align`'s `WA` underflow for a
/// FRAMEBUFFER/PALETTE address (both below `ram_base`) can never clamp into
/// this build's actual text window, so `HALT_CODE`'s SELF_MODIFY arm can
/// skip its runtime `NOT is_fb_or_pal_store` guard when it holds. `wa_safe`
/// computes `least(toUInt32(toUInt64(ADDR) - ram_base) >> 2, ram_words -
/// 1)`. For an address below `ram_base` (true of both regions), the
/// unsigned subtraction underflows and truncation of that wrapped value
/// equals `2**32 - (ram_base - addr)`. If that shifted value saturates the
/// `ram_words - 1` ceiling for the region's worst case, and that ceiling is
/// itself outside `[0, text_end_widx)`, the guard is provably dead weight.
/// False for a small fixture where `ram_words == text_end_widx`, which is
/// exactly the shape `test_framebuffer_store_does_not_trigger_self_modify`
/// stresses.
fn fb_pal_wa_provably_outside_text(ram_base: u32, ram_words: u32, text_end_widx: u32) -> bool {
    for base in [FRAMEBUFFER_BASE, PALETTE_BASE] {
        assert!(
            base < ram_base,
            "the SELF_MODIFY-arm optimization assumes region base {base:#x} is below ram_base {ram_base:#x}: it underflows WA on purpose"
        );
        let shifted = (((1u64 << 32) - (ram_base as u64 - base as u64)) & 0xFFFF_FFFF) >> 2;
        let shifted = shifted as u32;
        if shifted < ram_words - 1 || ram_words - 1 < text_end_widx {
            return false;
        }
    }
    true
}

/// The `arrayFold` lambda body: `(acc, i) -> tuple(...)`.
///
/// Accumulator (7-tuple): pc, regs\[31\], wl, control, icount, mmio,
/// fbpal_wl, where pc is a byte address, regs is x1..x31 (no x0 slot), wl =
/// tuple(addr\[\], val\[\], icount\[\]) (RAM's write-log), control =
/// tuple(stopped, halted, halt_reason, halt_pc, halt_extra), mmio =
/// tuple(console_bytes\[\], keyq_pos, tuple(frame_no, frame_committed)), and
/// fbpal_wl = tuple(fb_addr\[\], fb_val\[\], fb_icount\[\], pal_addr\[\],
/// pal_val\[\], pal_icount\[\]) — FRAMEBUFFER/PALETTE's own write-log lanes,
/// never scanned, kept separate from RAM's write-log because nothing ever
/// reads either region.
///
/// acc.5 is the ABSOLUTE icount (`UInt64`), not a per-batch retired count:
/// seeded from the batch's starting icount as `arrayFold`'s initial-value
/// argument (a runtime argument, not part of the compiled lambda text), and
/// incremented by 1 per retiring step. A per-batch-varying value baked
/// directly into the lambda body instead would sit inside ClickHouse's
/// compiled-expression cache key, so the lambda's text would differ every
/// batch and the JIT would never compile.
pub fn build_step(
    text_start_widx: u32,
    text_end_widx: u32,
    decn: u32,
    ram_words: u32,
    ram_base: u32,
    hwm: u32,
    ipms: u32,
) -> String {
    assert!(
        text_start_widx <= text_end_widx && text_end_widx <= ram_words,
        "text_start_widx={text_start_widx}/text_end_widx={text_end_widx} must be RAM_BASE-relative word indices with text_start_widx <= text_end_widx <= ram_words={ram_words}"
    );

    let fb_pal_wa_outside_text =
        fb_pal_wa_provably_outside_text(ram_base, ram_words, text_end_widx);

    let pc = "acc.1";
    let stopped = "acc.4.1";

    let idx = format!(
        "(least(bitShiftRight(toUInt32(toUInt64({pc}) - {ram_base}), 2), {}) + 1)",
        decn - 1
    );

    let id = format!("DEC[{idx}].1");
    let rd = format!("DEC[{idx}].2");
    let imm = format!("DEC[{idx}].5");
    let tgt = format!("DEC[{idx}].6");
    let dmkv = format!("DEC[{idx}].7");
    let dsgv = format!("DEC[{idx}].8");
    let raw = format!("DEC[{idx}].9");

    let r1 = format!("DEC[{idx}].3");
    let r2 = format!("DEC[{idx}].4");
    let a = format!("if({r1} = 0, toUInt32(0), acc.2[{r1}])");
    let b = format!("toUInt32(if({r2} = 0, toUInt32(0), acc.2[{r2}]) + {imm})");
    let rs2v = format!("if({r2} = 0, toUInt32(0), acc.2[{r2}])");
    let sa = format!("toInt32({a})");
    let sb = format!("toInt32({b})");

    let addr_align = addr_and_align(&a, &imm, &dmkv, ram_base, ram_words);
    let addr = addr_align.addr;
    let wa = addr_align.wa_safe;
    let misaligned_cond = addr_align.misaligned_cond;

    assert_eq!(MMIO_SIZE, 4096, "window mask below assumes a 4 KiB window");
    let is_mmio = format!(
        "(bitAnd({addr}, {}) = {MMIO_BASE})",
        0xFFFF_FFFFu32 ^ (MMIO_SIZE - 1)
    );

    let is_fb = format!("(toUInt32(toUInt64({addr}) - {FRAMEBUFFER_BASE}) < {FRAMEBUFFER_SIZE})");
    let is_pal = format!("(toUInt32(toUInt64({addr}) - {PALETTE_BASE}) < {PALETTE_SIZE})");
    let is_fb_or_pal_store = format!("({id}={OP_STORE} AND ({is_fb} OR {is_pal}))");
    let bad_addr_cond = format!(
        "({} AND NOT {is_mmio} AND NOT {is_fb_or_pal_store})",
        addr_align.bad_addr_cond
    );

    let mmio_is = |reg: u32| format!("({addr} = {})", MMIO_BASE + reg);

    let is_mmio_load = format!("({is_mmio} AND {id} = {OP_LOAD} AND {dmkv}=4294967295)");
    let is_mmio_store = format!("({is_mmio} AND {id} = {OP_STORE} AND {dmkv}=4294967295)");

    let ticks_ms = format!("toUInt32(intDiv(acc.5, {ipms}))");
    let keyq_has = "(acc.6.2 < toUInt32(length(KEYQT)))".to_string();
    let keyq_val = format!("if({keyq_has}, toUInt32(KEYQT[toUInt32(acc.6.2) + 1].1), toUInt32(0))");
    let mmio_read = format!(
        "multiIf({}, {ticks_ms}, {}, {keyq_val}, toUInt32(0))",
        mmio_is(clickdoom_spec::mmio::TICKS_MS),
        mmio_is(clickdoom_spec::mmio::KEYQ)
    );

    let sh = format!("(8 * bitAnd({addr}, 3))");

    let lw = format!(
        "if(arrayLastIndex(z -> z = {wa}, acc.3.1) > 0, acc.3.2[arrayLastIndex(z -> z = {wa}, acc.3.1)], RAMT[{wa} + 1].1)"
    );

    let extracted = format!("bitAnd(bitShiftRight({lw}, {sh}), {dmkv})");
    let sign_pos = format!("(bitShiftRight({dmkv}, 1) + 1)");
    let loadv = format!(
        "toUInt32({extracted} - if(bitAnd({extracted}, {sign_pos}) != 0 AND {dsgv} != 0, toUInt64({dmkv}) + 1, 0))"
    );

    let sval = format!(
        "toUInt32(bitOr(bitAnd({lw}, bitXor(4294967295, toUInt32(bitShiftLeft({dmkv}, {sh})))), toUInt32(bitShiftLeft(bitAnd({rs2v}, {dmkv}), {sh}))))"
    );

    let link_value = format!("toUInt32({pc} + 4)");
    let jalr_target = format!("bitAnd(toUInt32({a} + {imm}), 4294967294)");

    let loadv = format!(
        "multiIf({is_mmio} AND {dmkv}=4294967295, {mmio_read}, {is_mmio}, toUInt32(0), {loadv})"
    );

    let sa64 = format!("toInt64({sa})");
    let sb64 = format!("toInt64({sb})");

    let result = format!(
        "multiIf({id}=0, toUInt32({a} + {b}),\
         {id}=1, toUInt32({a} - {b}),\
         {id}=2, toUInt32(bitShiftLeft({a}, bitAnd({b},31))),\
         {id}=3, toUInt32({sa} < {sb}),\
         {id}=4, toUInt32({a} < {b}),\
         {id}=5, bitXor({a}, {b}),\
         {id}=6, toUInt32(bitShiftRight({a}, bitAnd({b},31))),\
         {id}=7, toUInt32(bitShiftRight({sa}, bitAnd({b},31))),\
         {id}=8, bitOr({a}, {b}),\
         {id}=9, bitAnd({a}, {b}),\
         {id}=10, toUInt32({sa} * {sb}),\
         {id}=11, toUInt32(bitShiftRight(toInt64({sa}) * toInt64({sb}), 32)),\
         {id}=12, toUInt32(bitShiftRight(toInt64({sa}) * toInt64({b}), 32)),\
         {id}=13, toUInt32(bitShiftRight(toUInt64({a}) * toUInt64({b}), 32)),\
         {id}=14, if({sb}=0, 4294967295, toUInt32(intDiv({sa64}, {sb64}))),\
         {id}=15, if({b}=0, 4294967295, toUInt32(intDiv({a}, {b}))),\
         {id}=16, if({sb}=0, {a}, toUInt32(modulo({sa64}, {sb64}))),\
         {id}=17, if({b}=0, {a}, toUInt32(modulo({a}, {b}))),\
         {id}={OP_LOAD}, {loadv},\
         {link_value})"
    );

    let fallthrough = format!("toUInt32({pc}+4)");
    let next = format!(
        "multiIf({id}=20, if({a} = {b},  {tgt}, {fallthrough}),\
         {id}=21, if({a} != {b}, {tgt}, {fallthrough}),\
         {id}=22, if({sa} < {sb},  {tgt}, {fallthrough}),\
         {id}=23, if({sa} >= {sb}, {tgt}, {fallthrough}),\
         {id}=24, if({a} < {b},  {tgt}, {fallthrough}),\
         {id}=25, if({a} >= {b}, {tgt}, {fallthrough}),\
         {id}=26, {tgt},\
         {id}=27, {jalr_target},\
         {fallthrough})"
    );

    let is_load = format!("{id}={OP_LOAD}");
    let is_store = format!("{id}={OP_STORE}");
    let is_mem = format!("({is_load} OR {is_store})");
    let is_ecall = format!("{id}={OP_ECALL}");
    let is_ebreak = format!("{id}={OP_EBREAK}");
    let is_csr = format!("{id}={OP_CSR}");
    let is_illegal = format!("{id}={OP_ILLEGAL}");

    let would_jump = format!(
        "multiIf({id}=20, {a} = {b},\
         {id}=21, {a} != {b},\
         {id}=22, {sa} < {sb},\
         {id}=23, {sa} >= {sb},\
         {id}=24, {a} < {b},\
         {id}=25, {a} >= {b},\
         {id}=26, true,\
         {id}=27, true,\
         false)"
    );
    let jump_target_if_taken = format!("if({id}=27, {jalr_target}, {tgt})");
    let is_jump_op = format!("({id} >= 20 AND {id} <= 27)");
    let jump_misaligned =
        format!("({is_jump_op} AND ({would_jump}) AND bitAnd({jump_target_if_taken}, 3) != 0)");

    let self_modify_extra_guard = if fb_pal_wa_outside_text {
        String::new()
    } else {
        format!(" AND NOT {is_fb_or_pal_store}")
    };

    let halt_code = format!(
        "multiIf({is_illegal}, {HALT_ILLEGAL_INSN},\
         {is_ecall}, {HALT_ECALL},\
         {is_ebreak}, {HALT_EBREAK},\
         {is_csr}, {HALT_CSR},\
         {jump_misaligned}, {HALT_MISALIGNED},\
         {is_mem} AND {bad_addr_cond}, {HALT_BAD_ADDR},\
         {is_mem} AND NOT {bad_addr_cond} AND {misaligned_cond}, {HALT_MISALIGNED},\
         {is_fb_or_pal_store} AND {dmkv} != 4294967295, {HALT_BAD_ADDR},\
         {is_store} AND NOT {bad_addr_cond} AND NOT {misaligned_cond} AND NOT {is_mmio}{self_modify_extra_guard} AND {wa} >= {text_start_widx} AND {wa} < {text_end_widx}, {HALT_SELF_MODIFY},\
         {is_mmio_store} AND NOT {misaligned_cond} AND {}, {HALT_EXIT},\
         {HALT_NONE})",
        mmio_is(clickdoom_spec::mmio::EXIT)
    );

    let active = format!("(NOT {stopped})");
    let hc = "hc";
    let step_halts_now = format!("({active} AND ({hc}) != 0)");
    let step_retires = format!("({active} AND ({hc}) = 0)");

    let halt_extra_calc = format!(
        "if(({hc}) = {HALT_ILLEGAL_INSN}, {raw}, if(({hc}) = {HALT_EXIT}, {rs2v}, if({jump_misaligned}, {jump_target_if_taken}, if(({hc}) IN ({HALT_BAD_ADDR}, {HALT_MISALIGNED}, {HALT_SELF_MODIFY}), {addr}, toUInt32(0)))))"
    );

    let is_retiring_store =
        format!("({step_retires} AND {is_store} AND NOT {is_mmio} AND NOT {is_fb_or_pal_store})");
    let new_wl_len_after_store = "(toUInt32(length(acc.3.1)) + 1)".to_string();
    let hits_hwm = format!("({is_retiring_store} AND {new_wl_len_after_store} >= {hwm})");

    let frame_committed_now = format!(
        "({step_retires} AND {is_mmio_store} AND {})",
        mmio_is(clickdoom_spec::mmio::FRAME_COMMIT)
    );

    let new_wl = format!(
        "if({is_retiring_store}, tuple(arrayPushBack(acc.3.1, {wa}), arrayPushBack(acc.3.2, {sval}), arrayPushBack(acc.3.3, acc.5 + 1)), acc.3)"
    );
    let new_control = format!(
        "multiIf({step_halts_now}, tuple(toUInt8(1), toUInt8(1), toUInt8({hc}), {pc}, {halt_extra_calc}), {hits_hwm} OR {frame_committed_now}, tuple(toUInt8(1), acc.4.2, acc.4.3, acc.4.4, acc.4.5), acc.4)"
    );

    let retiring_fb_store = format!("({step_retires} AND {id}={OP_STORE} AND {is_fb})");
    let retiring_pal_store = format!("({step_retires} AND {id}={OP_STORE} AND {is_pal})");
    let fb_wa = format!("bitShiftRight(toUInt32(toUInt64({addr}) - {FRAMEBUFFER_BASE}), 2)");
    let pal_wa = format!("bitShiftRight(toUInt32(toUInt64({addr}) - {PALETTE_BASE}), 2)");
    let new_fbpal_wl = format!(
        "tuple(\
         if({retiring_fb_store}, arrayPushBack(acc.7.1, {fb_wa}), acc.7.1),\
         if({retiring_fb_store}, arrayPushBack(acc.7.2, {rs2v}), acc.7.2),\
         if({retiring_fb_store}, arrayPushBack(acc.7.3, acc.5 + 1), acc.7.3),\
         if({retiring_pal_store}, arrayPushBack(acc.7.4, {pal_wa}), acc.7.4),\
         if({retiring_pal_store}, arrayPushBack(acc.7.5, {rs2v}), acc.7.5),\
         if({retiring_pal_store}, arrayPushBack(acc.7.6, acc.5 + 1), acc.7.6))"
    );

    let retiring_mmio_store = format!("({step_retires} AND {is_mmio_store})");

    let new_console = format!(
        "if({retiring_mmio_store} AND {}, arrayPushBack(acc.6.1, toUInt8(bitAnd({rs2v}, 255))), acc.6.1)",
        mmio_is(clickdoom_spec::mmio::PUTCHAR)
    );

    let new_keyq_pos = format!(
        "if({step_retires} AND {is_mmio_load} AND {} AND {keyq_has}, toUInt32(acc.6.2 + 1), acc.6.2)",
        mmio_is(clickdoom_spec::mmio::KEYQ)
    );

    let new_frame = format!("if({frame_committed_now}, tuple({rs2v}, toUInt8(1)), acc.6.3)");

    let new_mmio = format!("tuple({new_console}, {new_keyq_pos}, {new_frame})");

    let step_tuple_inner = format!(
        "tuple(\
         if({step_retires}, {next}, {pc}),\
         if({step_retires} AND {rd} != 0, arrayConcat(arraySlice(acc.2, 1, {rd} - 1), [toUInt32({result})], arraySlice(acc.2, {rd} + 1)), acc.2),\
         {new_wl},\
         {new_control},\
         if({step_retires}, acc.5 + 1, acc.5),\
         {new_mmio},\
         {new_fbpal_wl})"
    );
    format!("arrayMap({hc} -> {step_tuple_inner}, [{halt_code}])[1]")
}

/// The `WITH` clause materializing `RAMT`/`DEC`/`KEYQT`: one combined
/// `groupArray(tuple(...))` per table, not one `groupArray` per column, so
/// `optimize_read_in_order` cannot stream one column straight from its
/// physically sorted storage and silently misalign it against `word_addr`
/// while sibling columns, captured the same way in the same query, stay
/// correct.
pub fn decode_with(db: &str) -> String {
    format!(
        "\n  \
         (SELECT groupArray(tuple(value))\n     \
         FROM (SELECT value, word_addr FROM {db}.ram FINAL ORDER BY word_addr)) AS RAMT,\n  \
         (SELECT groupArray(tuple(id, rd, rs1, rs2, imm, tgt, mk, sg, raw))\n     \
         FROM (SELECT id, rd, rs1, rs2, imm, tgt, mk, sg, raw, word_addr\n           \
         FROM {db}.decoded ORDER BY word_addr)) AS DEC,\n  \
         (SELECT groupArray(tuple(key_event))\n     \
         FROM (SELECT key_event, event_seq FROM {db}.input_queue\n           \
         ORDER BY event_seq)) AS KEYQT"
    )
}

/// RAM's write-log seed, empty at the start of every real batch.
pub const WL0_EMPTY: &str = "tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64())";

fn init_acc(pc0: &str, regs0: &str, wl0: &str, icount0: &str, keyq0: &str) -> String {
    format!(
        "tuple(toUInt32({pc0}), {regs0}, {wl0}, tuple(toUInt8(0), toUInt8(0), toUInt8(0), toUInt32(0), toUInt32(0)), toUInt64({icount0}), tuple(emptyArrayUInt8(), toUInt32({keyq0}), tuple(toUInt32(0), toUInt8(0))), tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64(), emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64()))"
    )
}

/// Every optional argument [`select_only`] takes beyond the required
/// shape/K/hwm ones.
pub struct SelectOnlyArgs<'a> {
    pub pc0: Option<u32>,
    /// Each element is a SQL expression, not a bare value: a caller seeding
    /// an all-zero reset vector needs `toUInt32(0)` rather than `0`, since
    /// ClickHouse infers `Array(UInt8)` for an array literal whose values
    /// all happen to be small, and `arrayFold`'s accumulator then rejects
    /// it as a type mismatch. `regs.iter().map(u32::to_string)` is the
    /// right choice when the caller already knows the values fit and
    /// wants the plain literal instead.
    pub regs0: Option<&'a [String]>,
    pub db: &'a str,
    pub icount0: u64,
    pub keyq0: u32,
    pub ipms: u32,
    pub wl0: &'a str,
}

impl Default for SelectOnlyArgs<'_> {
    fn default() -> Self {
        SelectOnlyArgs {
            pc0: None,
            regs0: None,
            db: "clickdoom_executor",
            icount0: 0,
            keyq0: 0,
            ipms: clickdoom_spec::IPMS_DEFAULT,
            wl0: WL0_EMPTY,
        }
    }
}

/// Every optional argument [`batch`] takes beyond the required shape/K/hwm
/// ones.
pub struct BatchArgs<'a> {
    pub db: &'a str,
    pub ipms: u32,
}

impl Default for BatchArgs<'_> {
    fn default() -> Self {
        BatchArgs {
            db: "clickdoom_executor",
            ipms: clickdoom_spec::IPMS_DEFAULT,
        }
    }
}

/// The fold alone: no state reload, no commit, nothing written.
///
/// `args.wl0` seeds acc.3, RAM's write-log — measuring whether per-step
/// cost is a function of write-log length requires varying that length
/// independently of K, which nothing in normal operation can do (the log
/// only grows by retiring stores). [`crate::fold::batch`] deliberately does
/// not take this parameter, and must not: `commit::ram_flush_sql` flushes
/// `wl_addr` straight into `ram.word_addr`, so a seeded log on the
/// end-to-end path would insert rows at whatever synthetic word addresses
/// the seed used, corrupting RAM rather than failing. The parameter's
/// absence from `batch` is the guard.
pub fn select_only(
    k: u32,
    text_start_widx: u32,
    text_end_widx: u32,
    decn: u32,
    ram_words: u32,
    hwm: u32,
    args: &SelectOnlyArgs,
) -> String {
    let step = build_step(
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        RAM_BASE,
        hwm,
        args.ipms,
    );
    let regs0_sql = match args.regs0 {
        Some(regs) => format!("[{}]", regs.join(",")),
        None => "arrayResize(emptyArrayUInt32(), 31, toUInt32(0))".to_string(),
    };
    let pc0 = args.pc0.unwrap_or(RAM_BASE);
    let init = init_acc(
        &pc0.to_string(),
        &regs0_sql,
        args.wl0,
        &args.icount0.to_string(),
        &args.keyq0.to_string(),
    );
    let db = args.db;
    let icount0 = args.icount0;
    format!(
        "WITH{}\n\
         SELECT r.1 AS pc, r.2 AS regs, r.3.1 AS wl_addr, r.3.2 AS wl_val, r.3.3 AS wl_icount,\n       \
         r.4.1 AS stopped, r.4.2 AS halted, r.4.3 AS halt_reason, r.4.4 AS halt_pc,\n       \
         r.4.5 AS halt_extra, toUInt32(r.5 - toUInt64({icount0})) AS retired,\n       \
         r.6.1 AS console_bytes, r.6.2 AS keyq_pos, r.6.3.1 AS frame_no, r.6.3.2 AS frame_committed,\n       \
         r.7.1 AS fb_wl_addr, r.7.2 AS fb_wl_val, r.7.3 AS fb_wl_icount,\n       \
         r.7.4 AS pal_wl_addr, r.7.5 AS pal_wl_val, r.7.6 AS pal_wl_icount\n\
         FROM (SELECT arrayFold((acc, i) -> {step}, range({k}), {init}) AS r)\n\
         SETTINGS max_threads = 1,\n         \
         max_ast_elements = 500000, max_expanded_ast_elements = 500000,\n         \
         max_query_size = 2000000",
        decode_with(db)
    )
}

/// `transform(halt_code_expr, [1..8], ['ILLEGAL_INSN', ..., 'EXIT'], '')`,
/// generated from [`HALT_REASON_NAMES`] so the SQL mapping can't drift from
/// the Rust one. `HALT_NONE` isn't in the from-array, so it (and anything
/// else unrecognized) falls through to the `''` default, matching
/// `HALT_NONE`'s own name anyway. Evaluated once per batch, outside the
/// fold lambda.
pub fn halt_reason_transform(halt_code_expr: &str) -> String {
    let from_arr = format!(
        "[{}]",
        HALT_REASON_NAMES
            .iter()
            .map(|(code, _)| code.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let to_arr = format!(
        "[{}]",
        HALT_REASON_NAMES
            .iter()
            .map(|(_, name)| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(",")
    );
    format!("transform(toUInt8({halt_code_expr}), {from_arr}, {to_arr}, '')")
}

/// One full batch: reload prior state from `batch_commit`, fold up to K
/// instructions, and INSERT the resulting `batch_commit` row directly. This
/// INSERT is the batch's single atomic write. Flushing wl_*/console_bytes
/// into `ram`/`console_out` and deriving `cpu_state` are separate,
/// idempotent statements ([`crate::commit`]), deliberately not here, since
/// they must be safely re-runnable independently of this INSERT ever having
/// happened more than once.
pub fn batch(
    k: u32,
    text_start_widx: u32,
    text_end_widx: u32,
    decn: u32,
    ram_words: u32,
    hwm: u32,
    args: &BatchArgs,
) -> String {
    let db = args.db;
    let step = build_step(
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        RAM_BASE,
        hwm,
        args.ipms,
    );
    let init = init_acc(
        "assumeNotNull(PREV.2)",
        "CAST(PREV.3, 'Array(UInt32)')",
        WL0_EMPTY,
        "assumeNotNull(PREV.4)",
        "assumeNotNull(PREV.5)",
    );
    let halt_reason_expr = halt_reason_transform("r.4.3");
    let exit_code_expr = format!("if(toUInt8(r.4.3) = {HALT_EXIT}, r.4.5, toUInt32(0))");
    format!(
        "INSERT INTO {db}.batch_commit\n  \
         (batch_id, icount, pc, regs, halted, halt_reason, exit_code,\n   \
         keyq_pos, has_frame, frame_no, wl_addr, wl_val, wl_icount,\n   \
         fb_wl_addr, fb_wl_val, fb_wl_icount, pal_wl_addr, pal_wl_val, pal_wl_icount,\n   \
         console_bytes)\n\
         WITH{},\n  \
         (SELECT tuple(batch_id, pc, regs, icount, keyq_pos)\n     \
         FROM {db}.batch_commit ORDER BY batch_id DESC LIMIT 1) AS PREV\n\
         SELECT toUInt64(assumeNotNull(PREV.1) + 1) AS batch_id,\n       \
         r.5 AS icount,\n       \
         r.1 AS pc, r.2 AS regs,\n       \
         r.4.2 AS halted, {halt_reason_expr} AS halt_reason, {exit_code_expr} AS exit_code,\n       \
         r.6.2 AS keyq_pos, r.6.3.2 AS has_frame, r.6.3.1 AS frame_no,\n       \
         r.3.1 AS wl_addr, r.3.2 AS wl_val, r.3.3 AS wl_icount,\n       \
         r.7.1 AS fb_wl_addr, r.7.2 AS fb_wl_val, r.7.3 AS fb_wl_icount,\n       \
         r.7.4 AS pal_wl_addr, r.7.5 AS pal_wl_val, r.7.6 AS pal_wl_icount,\n       \
         r.6.1 AS console_bytes\n\
         FROM (SELECT arrayFold((acc, i) -> {step}, range({k}), {init}) AS r)\n\
         SETTINGS max_threads = 1,\n         \
         max_ast_elements = 500000, max_expanded_ast_elements = 500000,\n         \
         max_query_size = 2000000",
        decode_with(db)
    )
}
