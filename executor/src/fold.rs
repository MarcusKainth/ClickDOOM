//! The batch fold: SPEC halt semantics, early termination (halt, write-log
//! high-water mark, FRAME_COMMIT), the write-log versioning fix (every
//! write-log entry carries its own retiring instruction's icount, not the
//! batch's final icount), and the register checkpoints the batch crosses.

use clickdoom_spec::{
    CHECKPOINT_INTERVAL, FRAMEBUFFER_BASE, FRAMEBUFFER_SIZE, MMIO_BASE, MMIO_SIZE, PALETTE_BASE,
    PALETTE_SIZE, RAM_BASE,
};

use crate::word::Widx;

use crate::config::{
    HALT_BAD_ADDR, HALT_CSR, HALT_EBREAK, HALT_ECALL, HALT_EXIT, HALT_ILLEGAL_INSN,
    HALT_MISALIGNED, HALT_NONE, HALT_REASON_NAMES, HALT_SELF_MODIFY, OP_CSR, OP_EBREAK, OP_ECALL,
    OP_ILLEGAL, OP_LOAD, OP_STORE,
};

/// Which formulation of the step [`build_step_variant`] emits. Every arm
/// computes the same accumulator from the same inputs. They differ in how
/// much expression text the planner prints into each action node's
/// `result_name`, and in how many distinct constants the lambda captures.
/// [`Variant::Baseline`] is the one production runs.
///
/// Every arm reaches the server through the same `SETTINGS` clause as the
/// baseline, so the totality rule stated on `FOLD_SETTINGS` covers all of
/// them and not only [`Variant::Baseline`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    /// The formulation [`build_step`] emits.
    #[default]
    Baseline,
    /// `halt_code` written out at every site that reads it, with no
    /// `arrayMap` binding around the step tuple. Structural dedup keeps the
    /// node count close to the baseline's while the printed text grows.
    InlineHaltCode,
    /// The `arrayMap` binding kept with a one-character lambda parameter.
    /// The DAG is the baseline's node for node.
    ShortBindingParam,
    /// One further `arrayMap` binding for the decode row, nested outside
    /// the `halt_code` binding. That is the step's most repeated
    /// subexpression, and one binding is one further lambda scope for the
    /// captured decode and RAM arrays to pass through.
    BindDecodeRow,
    /// [`Variant::BindDecodeRow`]'s binding plus one for each of the other
    /// repeated subexpressions the step reads: both register operands, the
    /// store value, the byte address, the word address and the loaded RAM
    /// word.
    BindRepeated,
    /// The same result reached from fewer distinct captured constants.
    /// [`FEWER_CONSTANTS_ASSUMES_MK`] states the one input-domain
    /// assumption this arm carries that the baseline does not.
    FewerConstants,
    /// [`ADDED_CONSTANTS`] extra distinct captured constants, in `multiIf`
    /// arms that the halt-code domain makes unreachable.
    MoreConstants,
}

/// [`Variant::FewerConstants`] derives the load/store alignment mask from
/// `mk`'s bit pattern instead of comparing against each width, which holds
/// for the values `sqlcpu/decode.sql` writes into the column and for no
/// others.
pub const FEWER_CONSTANTS_ASSUMES_MK: [u32; 4] = [0, 255, 65_535, 4_294_967_295];

/// How many extra distinct constants [`Variant::MoreConstants`] captures,
/// and the first halt-code value its unreachable arms compare against. The
/// halt codes `config` defines stop at [`HALT_EXIT`], so no arm fires.
pub const ADDED_CONSTANTS: u8 = 60;
const ADDED_CONSTANTS_FIRST: u8 = 33;

/// What [`build_step_inner`] returns: the step itself, or the same
/// expressions with every lambda binding replaced by a free column
/// reference. No `EXPLAIN` form descends into a lambda, so the flat form
/// is the only way to read one ActionsDAG holding every action node the
/// step needs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Emit {
    Nested,
    Flat,
    /// The step with its `n` outermost bindings left free and the rest
    /// still nested. `EXPLAIN` over this prints scope `n`'s own DAG, which
    /// is what one `ExpressionActions` run executes per fold step.
    Peel(usize),
}

/// Collects the `arrayMap` bindings a variant introduces, outermost first.
/// [`Binder::bind`] returns the name to read the expression by, which is
/// the expression itself when `on` is false, so a caller's text is
/// byte-identical to the baseline's for a variant that binds nothing.
struct Binder {
    bindings: Vec<(&'static str, String)>,
}

impl Binder {
    fn bind(&mut self, on: bool, name: &'static str, expr: String) -> String {
        if !on {
            return expr;
        }
        self.bindings.push((name, expr));
        name.to_string()
    }

    /// `body` wrapped in one `arrayMap` per binding, so a binding pushed
    /// later sits inside one pushed earlier and may read it. `peel` leaves
    /// that many outermost bindings unwrapped, so their names stay free
    /// column references.
    fn wrap_peeled(self, body: String, peel: usize) -> String {
        let kept = self.bindings.len().saturating_sub(peel);
        self.bindings
            .into_iter()
            .skip(peel)
            .take(kept)
            .rev()
            .fold(body, |inner, (name, expr)| {
                format!("arrayMap({name} -> {inner}, [{expr}])[1]")
            })
    }

    fn wrap(self, body: String) -> String {
        self.wrap_peeled(body, 0)
    }

    /// `body` and every bound expression as one comma-separated list, for
    /// [`Emit::Flat`].
    fn flatten(self, body: String) -> String {
        let mut parts = vec![body];
        parts.extend(self.bindings.into_iter().map(|(_, expr)| expr));
        parts.join(", ")
    }
}

/// The column names [`build_step_flat`] leaves free, in the types
/// `step_variants`'s probe table has to declare them with.
pub const FLAT_COLUMNS: &[(&str, &str)] = &[
    ("hc", "UInt8"),
    ("h", "UInt8"),
    (
        "d",
        "Tuple(UInt8, UInt8, UInt8, UInt8, UInt32, UInt32, UInt32, UInt8, UInt32)",
    ),
    ("va", "UInt32"),
    ("vb", "UInt32"),
    ("vr2", "UInt32"),
    ("ad", "UInt32"),
    ("wx", "UInt32"),
    ("mw", "UInt32"),
];

/// `(ADDR, bad_addr_cond, misaligned_cond, WA_safe)`. `wa_safe` is masked
/// into `[0, ram_words)` unconditionally, so `arrayElement` on RAM/write-log
/// arrays never throws regardless of whether the access is actually in
/// range; `bad_addr_cond` is the real (unmasked) bounds check used for halt
/// detection, computed independently.
struct AddrAndAlign {
    bad_addr_cond: String,
    misaligned_cond: String,
    wa_safe: String,
}

/// `addr` is the byte address the caller has already built, so a variant
/// that binds it to a lambda parameter passes the parameter here.
fn addr_and_align(
    addr: &str,
    dmkv: &str,
    ram_base: u32,
    ram_words: u32,
    variant: Variant,
) -> AddrAndAlign {
    let ram_off = format!("bitShiftRight(toUInt32(toUInt64({addr}) - {ram_base}), 2)");
    let wa_safe = format!("least({ram_off}, {})", ram_words - 1);
    let bad_addr_cond = if variant == Variant::FewerConstants {
        // `addr` is outside `[ram_base, ram_base + 4 * ram_words)` exactly
        // when the wrapped word offset exceeds the last word index: an
        // address below `ram_base` wraps to at least `2**32 - ram_base`,
        // which the caller's assert keeps above `ram_words - 1`.
        format!("({ram_off} > {})", ram_words - 1)
    } else {
        let addr64 = format!("toUInt64({addr})");
        let ram_end = ram_base + ram_words * 4;
        format!("({addr64} < {ram_base} OR {addr64} >= {ram_end})")
    };
    let align_mask = if variant == Variant::FewerConstants {
        // Bit 15 is set for the 16- and 32-bit masks, bit 31 only for the
        // 32-bit one, so this reads 0, 1 and 3 off `mk` directly.
        format!("bitOr(bitAnd(bitShiftRight({dmkv}, 15), 1), bitAnd(bitShiftRight({dmkv}, 30), 2))")
    } else {
        format!("multiIf({dmkv}=4294967295, 3, {dmkv}=65535, 1, 0)")
    };
    let misaligned_cond = format!("(bitAnd({addr}, {align_mask}) != 0)");
    AddrAndAlign {
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

/// How many registers a checkpoint records, one per x1..x31. `cp_regs`
/// holds this many words per entry, in register order, so entry `n`
/// occupies `arraySlice(cp_regs, (n - 1) * CHECKPOINT_REGS + 1,
/// CHECKPOINT_REGS)`.
pub const CHECKPOINT_REGS: u32 = 31;

/// The `arrayFold` lambda body: `(acc, i) -> tuple(...)`.
///
/// Accumulator (8-tuple): pc, regs\[31\], wl, control, icount, mmio,
/// fbpal_wl, cp, where pc is a byte address, regs is x1..x31 (no x0 slot),
/// wl = tuple(addr\[\], val\[\], icount\[\]) (RAM's write-log), control =
/// tuple(stopped, halted, halt_reason, halt_pc, halt_extra), mmio =
/// tuple(console_bytes\[\], keyq_pos, tuple(frame_no, frame_committed)),
/// fbpal_wl = tuple(fb_addr\[\], fb_val\[\], fb_icount\[\], pal_addr\[\],
/// pal_val\[\], pal_icount\[\]) — FRAMEBUFFER/PALETTE's own write-log lanes,
/// never scanned, kept separate from RAM's write-log because nothing ever
/// reads either region — and cp = tuple(cp_icount\[\], cp_pc\[\],
/// cp_regs\[\]), the register checkpoints the batch crossed.
///
/// A checkpoint is appended by the step that follows the one retiring an
/// instruction whose count is a multiple of `CHECKPOINT_INTERVAL`, reading
/// pc, the register file and the count straight off the incoming
/// accumulator. `cp_regs` is flat: [`CHECKPOINT_REGS`] words per entry, in
/// the same order as `cp_icount`.
///
/// A boundary landing on the batch's own last retired instruction has no
/// following step and so is not in `cp`. It is the batch's committed
/// `cpu_state` row, which the caller already has.
///
/// acc.5 is the ABSOLUTE icount (`UInt64`), not a per-batch retired count:
/// seeded from the batch's starting icount as `arrayFold`'s initial-value
/// argument (a runtime argument, not part of the compiled lambda text), and
/// incremented by 1 per retiring step. A per-batch-varying value baked
/// directly into the lambda body instead would sit inside ClickHouse's
/// compiled-expression cache key, so the lambda's text would differ every
/// batch and the JIT would never compile.
pub fn build_step(
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    ram_base: u32,
    hwm: u32,
    ipms: u32,
) -> String {
    build_step_variant(
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        ram_base,
        hwm,
        ipms,
        Variant::Baseline,
    )
}

/// [`build_step`] in the formulation `variant` names. Every arm computes
/// the same accumulator; [`Variant`] says what each one moves.
#[allow(clippy::too_many_arguments)]
pub fn build_step_variant(
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    ram_base: u32,
    hwm: u32,
    ipms: u32,
    variant: Variant,
) -> String {
    build_step_inner(
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        ram_base,
        hwm,
        ipms,
        variant,
        Emit::Nested,
    )
}

/// [`build_step_variant`]'s expressions as a SELECT list, with every lambda
/// binding replaced by a reference to the [`FLAT_COLUMNS`] column of the
/// same name. `EXPLAIN actions = 1` over this prints one ActionsDAG holding
/// every action node the step needs, across the lambda scopes the nested
/// form splits them over.
#[allow(clippy::too_many_arguments)]
pub fn build_step_flat(
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    ram_base: u32,
    hwm: u32,
    ipms: u32,
    variant: Variant,
) -> String {
    build_step_inner(
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        ram_base,
        hwm,
        ipms,
        variant,
        Emit::Flat,
    )
}

/// [`build_step_variant`] with its `peel` outermost bindings left as free
/// [`FLAT_COLUMNS`] references. `EXPLAIN actions = 1` over this prints the
/// DAG of the scope that binding `peel` opens, which is one of the
/// `ExpressionActions` a fold step runs. Summing over `peel` from 0 up to
/// the arm's binding count gives the step's whole per-scope cost.
#[allow(clippy::too_many_arguments)]
pub fn build_step_peeled(
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    ram_base: u32,
    hwm: u32,
    ipms: u32,
    variant: Variant,
    peel: usize,
) -> String {
    build_step_inner(
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        ram_base,
        hwm,
        ipms,
        variant,
        Emit::Peel(peel),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_step_inner(
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    ram_base: u32,
    hwm: u32,
    ipms: u32,
    variant: Variant,
    emit: Emit,
) -> String {
    let text_start_widx = text_start_widx.get();
    let text_end_widx = text_end_widx.get();
    assert!(
        text_start_widx <= text_end_widx && text_end_widx <= ram_words,
        "text_start_widx={text_start_widx}/text_end_widx={text_end_widx} are not inside a \
         {ram_words}-word region. A bound larger than the region is an absolute word address \
         that was never rebased against the image's own base word"
    );
    assert!(
        ipms != 0,
        "ipms is 0, which makes the TICKS_MS read divide by zero"
    );
    if variant == Variant::FewerConstants {
        assert!(
            text_end_widx >= 1,
            "FewerConstants rewrites the text-window bound as wa <= text_end_widx - 1"
        );
        assert!(
            (ram_base as u64) + (ram_words as u64) * 4 <= 1u64 << 32,
            "FewerConstants reads the RAM bounds check off a wrapped word offset, which needs RAM to end at or below 2**32"
        );
    }

    let fb_pal_wa_outside_text =
        fb_pal_wa_provably_outside_text(ram_base, ram_words, text_end_widx);

    // BindDecodeRow takes the first binding only; BindRepeated takes all
    // of them.
    let bind_all = variant == Variant::BindRepeated;
    let bind_dec = bind_all || variant == Variant::BindDecodeRow;
    let mut binder = Binder {
        bindings: Vec::new(),
    };

    let pc = "acc.1";
    let stopped = "acc.4.1";

    let pc_widx = format!("bitShiftRight(toUInt32(toUInt64({pc}) - {ram_base}), 2)");
    let idx = format!("(least({pc_widx}, {}) + 1)", decn - 1);

    // `DEC` holds the text region and nothing else, and the clamp above
    // keeps `arrayElement` in bounds rather than deciding what a pc outside
    // that region means. This is what decides it: the fetch halts, so the
    // clamped row never executes.
    //
    // An empty window declares no region to be outside of, and a caller
    // that wants every word of RAM fetchable passes one. A window starting
    // at 0 needs no lower test: a pc below `ram_base` wraps the unsigned
    // subtraction to a value past the region's end.
    let pc_out_of_text = if text_end_widx == text_start_widx {
        None
    } else if text_start_widx == 0 {
        Some(format!("({pc_widx} >= {text_end_widx})"))
    } else {
        Some(format!(
            "({pc_widx} < {text_start_widx} OR {pc_widx} >= {text_end_widx})"
        ))
    };
    let fetch_halt_arm = match &pc_out_of_text {
        Some(cond) => format!("{cond}, {HALT_BAD_ADDR},"),
        None => String::new(),
    };

    let dec = binder.bind(bind_dec, "d", format!("DEC[{idx}]"));

    let id = format!("{dec}.1");
    let rd = format!("{dec}.2");
    let imm = format!("{dec}.5");
    let tgt = format!("{dec}.6");
    let dmkv = format!("{dec}.7");
    let dsgv = format!("{dec}.8");
    let raw = format!("{dec}.9");

    let r1 = format!("{dec}.3");
    let r2 = format!("{dec}.4");
    let one8 = if variant == Variant::FewerConstants {
        "1"
    } else {
        "toUInt8(1)"
    };
    let a = binder.bind(
        bind_all,
        "va",
        format!("if({r1} = 0, toUInt32(0), acc.2[{r1}])"),
    );
    let b = binder.bind(
        bind_all,
        "vb",
        format!("toUInt32(if({r2} = 0, toUInt32(0), acc.2[{r2}]) + {imm})"),
    );
    let rs2v = binder.bind(
        bind_all,
        "vr2",
        format!("if({r2} = 0, toUInt32(0), acc.2[{r2}])"),
    );
    let sa = format!("toInt32({a})");
    let sb = format!("toInt32({b})");

    let addr = binder.bind(bind_all, "ad", format!("toUInt32({a} + {imm})"));
    let addr_align = addr_and_align(&addr, &dmkv, ram_base, ram_words, variant);
    let wa = binder.bind(bind_all, "wx", addr_align.wa_safe);
    let misaligned_cond = addr_align.misaligned_cond;

    assert_eq!(MMIO_SIZE, 4096, "window mask below assumes a 4 KiB window");
    let mmio_off = format!("toUInt32(toUInt64({addr}) - {MMIO_BASE})");
    let is_mmio = if variant == Variant::FewerConstants {
        format!("({mmio_off} < {MMIO_SIZE})")
    } else {
        format!(
            "(bitAnd({addr}, {}) = {MMIO_BASE})",
            0xFFFF_FFFFu32 ^ (MMIO_SIZE - 1)
        )
    };

    let is_fb = format!("(toUInt32(toUInt64({addr}) - {FRAMEBUFFER_BASE}) < {FRAMEBUFFER_SIZE})");
    let is_pal = format!("(toUInt32(toUInt64({addr}) - {PALETTE_BASE}) < {PALETTE_SIZE})");
    let is_fb_or_pal_store = format!("({id}={OP_STORE} AND ({is_fb} OR {is_pal}))");
    let bad_addr_cond = format!(
        "({} AND NOT {is_mmio} AND NOT {is_fb_or_pal_store})",
        addr_align.bad_addr_cond
    );

    // Every call site sits under a condition that already implies the
    // address is inside the MMIO window, so comparing the window offset
    // against the register offset decides the same thing as comparing the
    // full address against the register address.
    let mmio_is = |reg: u32| {
        if variant == Variant::FewerConstants {
            format!("({mmio_off} = {reg})")
        } else {
            format!("({addr} = {})", MMIO_BASE + reg)
        }
    };

    let is_mmio_load = format!("({is_mmio} AND {id} = {OP_LOAD} AND {dmkv}=4294967295)");
    let is_mmio_store = format!("({is_mmio} AND {id} = {OP_STORE} AND {dmkv}=4294967295)");

    let ticks_ms = format!("toUInt32(intDiv(acc.5, {ipms}))");
    let keyq_has = "(acc.6.2 < toUInt32(length(KEYQT)))".to_string();
    let keyq_val = format!("if({keyq_has}, toUInt32(KEYQT[toUInt32(acc.6.2) + 1]), toUInt32(0))");
    let mmio_read = format!(
        "multiIf({}, {ticks_ms}, {}, {keyq_val}, toUInt32(0))",
        mmio_is(clickdoom_spec::mmio::TICKS_MS),
        mmio_is(clickdoom_spec::mmio::KEYQ)
    );

    let sh = format!("(8 * bitAnd({addr}, 3))");

    let lw = binder.bind(
        bind_all,
        "mw",
        format!(
            "if(arrayLastIndex(z -> z = {wa}, acc.3.1) > 0, acc.3.2[arrayLastIndex(z -> z = {wa}, acc.3.1)], RAMT[{wa} + 1])"
        ),
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
    // Shifting the low bit out and back clears it without naming the mask.
    let jalr_target = if variant == Variant::FewerConstants {
        format!("toUInt32(bitShiftLeft(bitShiftRight({addr}, 1), 1))")
    } else {
        format!("bitAnd({addr}, 4294967294)")
    };

    let loadv = format!(
        "multiIf({is_mmio} AND {dmkv}=4294967295, {mmio_read}, {is_mmio}, toUInt32(0), {loadv})"
    );

    let sa64 = format!("toInt64({sa})");
    let sb64 = format!("toInt64({sb})");

    // Divisors the divide and remainder arms can evaluate for any input.
    // ClickHouse decides per session whether an `if` arm holding a division
    // runs when its guard is false, and `rs2 = x0` makes the second operand
    // 0 on ordinary instructions. Substituting 1 for a zero divisor changes
    // no result: the enclosing `if` returns RISC-V's div-by-zero and
    // rem-by-zero values from its own arm. The signed divisor stays Int64,
    // so `DIV(INT_MIN, -1)` gives INT_MIN rather than overflowing Int32.
    let nz_b = format!("if({b}=0, toUInt32(1), {b})");
    let nz_sb64 = format!("if({sb}=0, toInt64(1), {sb64})");

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
         {id}=14, if({sb}=0, 4294967295, toUInt32(intDiv({sa64}, {nz_sb64}))),\
         {id}=15, if({b}=0, 4294967295, toUInt32(intDiv({a}, {nz_b}))),\
         {id}=16, if({sb}=0, {a}, toUInt32(modulo({sa64}, {nz_sb64}))),\
         {id}=17, if({b}=0, {a}, toUInt32(modulo({a}, {nz_b}))),\
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

    // `Bool`'s own literals cost two more captured constants than the
    // `UInt8` ones every comparison arm above already needs.
    let (jump_yes, jump_no) = if variant == Variant::FewerConstants {
        ("1", "0")
    } else {
        ("true", "false")
    };
    let would_jump = format!(
        "multiIf({id}=20, {a} = {b},\
         {id}=21, {a} != {b},\
         {id}=22, {sa} < {sb},\
         {id}=23, {sa} >= {sb},\
         {id}=24, {a} < {b},\
         {id}=25, {a} >= {b},\
         {id}=26, {jump_yes},\
         {id}=27, {jump_yes},\
         {jump_no})"
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

    // `wa <= text_end_widx - 1` decides the same thing as `wa <
    // text_end_widx` and shares its constant with the decode-table clamp
    // whenever the text window ends where the decode table does.
    let text_upper = if variant == Variant::FewerConstants {
        format!("{wa} <= {}", text_end_widx - 1)
    } else {
        format!("{wa} < {text_end_widx}")
    };
    // `multiIf` takes the first match, so arm order is the halt-reason
    // precedence. The fetch is tested first, because a step whose
    // instruction was never fetched has no opcode to classify. Misalignment
    // is then tested before the address's region and before the access's
    // width, so an access that is both misaligned and outside every region
    // is `MISALIGNED`, and so is a misaligned narrow store to FRAMEBUFFER
    // or PALETTE.
    let halt_code = format!(
        "multiIf({fetch_halt_arm}{is_illegal}, {HALT_ILLEGAL_INSN},\
         {is_ecall}, {HALT_ECALL},\
         {is_ebreak}, {HALT_EBREAK},\
         {is_csr}, {HALT_CSR},\
         {jump_misaligned}, {HALT_MISALIGNED},\
         {is_mem} AND {misaligned_cond}, {HALT_MISALIGNED},\
         {is_mem} AND {bad_addr_cond}, {HALT_BAD_ADDR},\
         {is_fb_or_pal_store} AND {dmkv} != 4294967295, {HALT_BAD_ADDR},\
         {is_store} AND NOT {bad_addr_cond} AND NOT {misaligned_cond} AND NOT {is_mmio}{self_modify_extra_guard} AND {wa} >= {text_start_widx} AND {text_upper}, {HALT_SELF_MODIFY},\
         {is_mmio_store} AND NOT {misaligned_cond} AND {}, {HALT_EXIT},\
         {HALT_NONE})",
        mmio_is(clickdoom_spec::mmio::EXIT)
    );

    let active = format!("(NOT {stopped})");
    let hc_param = if variant == Variant::ShortBindingParam {
        "h"
    } else {
        "hc"
    };
    let hc: &str = match variant {
        Variant::InlineHaltCode => &halt_code,
        _ => hc_param,
    };
    let step_halts_now = format!("({active} AND ({hc}) != 0)");
    let step_retires = format!("({active} AND ({hc}) = 0)");

    let halt_extra_calc = format!(
        "if(({hc}) = {HALT_ILLEGAL_INSN}, {raw}, if(({hc}) = {HALT_EXIT}, {rs2v}, if({jump_misaligned}, {jump_target_if_taken}, if(({hc}) IN ({HALT_BAD_ADDR}, {HALT_MISALIGNED}, {HALT_SELF_MODIFY}), {addr}, toUInt32(0)))))"
    );
    // A fetch that never happened has no data address, so the address the
    // record carries is the pc itself.
    let halt_extra_calc = match &pc_out_of_text {
        Some(cond) => format!("if({cond}, {pc}, {halt_extra_calc})"),
        None => halt_extra_calc,
    };
    let halt_extra_calc = if variant == Variant::MoreConstants {
        let dead: String = (0..ADDED_CONSTANTS)
            .map(|n| format!("({hc}) = {}, toUInt32(0), ", ADDED_CONSTANTS_FIRST + n))
            .collect();
        format!("multiIf({dead}{halt_extra_calc})")
    } else {
        halt_extra_calc
    };

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
        "multiIf({step_halts_now}, tuple({one8}, {one8}, toUInt8({hc}), {pc}, {halt_extra_calc}), {hits_hwm} OR {frame_committed_now}, tuple({one8}, acc.4.2, acc.4.3, acc.4.4, acc.4.5), acc.4)"
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

    // Read off the incoming accumulator, one step after the boundary
    // instruction retired, so pc, the register file and the count are the
    // values that boundary left behind. `i > 0` keeps a batch that starts
    // on a boundary from recording the entry the batch before it recorded.
    let at_checkpoint = format!(
        "((NOT {stopped}) AND i > 0 AND acc.5 != 0 AND modulo(acc.5, {CHECKPOINT_INTERVAL}) = 0)"
    );
    let new_cp = format!(
        "if({at_checkpoint}, \
         tuple(arrayPushBack(acc.8.1, acc.5), arrayPushBack(acc.8.2, {pc}), arrayConcat(acc.8.3, acc.2)), \
         acc.8)"
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

    let new_frame = format!("if({frame_committed_now}, tuple({rs2v}, {one8}), acc.6.3)");

    let new_mmio = format!("tuple({new_console}, {new_keyq_pos}, {new_frame})");

    let step_tuple_inner = format!(
        "tuple(\
         if({step_retires}, {next}, {pc}),\
         if({step_retires} AND {rd} != 0, arrayConcat(arraySlice(acc.2, 1, {rd} - 1), [toUInt32({result})], arraySlice(acc.2, {rd} + 1)), acc.2),\
         {new_wl},\
         {new_control},\
         if({step_retires}, acc.5 + 1, acc.5),\
         {new_mmio},\
         {new_fbpal_wl},\
         {new_cp})"
    );
    // The `halt_code` binding is the innermost one, so the probes reach its
    // scope the same way they reach a variant's own bindings.
    if variant != Variant::InlineHaltCode {
        binder.bindings.push((hc_param, halt_code));
    }
    match emit {
        Emit::Flat => binder.flatten(step_tuple_inner),
        Emit::Nested => binder.wrap(step_tuple_inner),
        Emit::Peel(n) => binder.wrap_peeled(step_tuple_inner, n),
    }
}

/// The `WITH` clause materializing `RAMT`/`DEC`/`KEYQT`.
///
/// `DEC` captures its columns as one `groupArray(tuple(...))`. Separate
/// `groupArray`s over one subquery let `optimize_read_in_order` stream a
/// column straight from its physically sorted storage and silently
/// misalign it against `word_addr` while its sibling columns stay correct.
/// A single tuple aggregate carries every column, so they stay aligned.
///
/// `RAMT` and `KEYQT` capture one column each, so they have no sibling
/// column to misalign against and use a bare `groupArray`.
/// `driver/src/checkpoint.rs` captures `ram` the same way.
pub fn decode_with(db: &str) -> String {
    format!(
        "\n  \
         (SELECT groupArray(value)\n     \
         FROM (SELECT value, word_addr FROM {db}.ram FINAL ORDER BY word_addr)) AS RAMT,\n  \
         (SELECT groupArray(tuple(id, rd, rs1, rs2, imm, tgt, mk, sg, raw))\n     \
         FROM (SELECT id, rd, rs1, rs2, imm, tgt, mk, sg, raw, word_addr\n           \
         FROM {db}.decoded ORDER BY word_addr)) AS DEC,\n  \
         (SELECT groupArray(key_event)\n     \
         FROM (SELECT key_event, event_seq FROM {db}.input_queue\n           \
         ORDER BY event_seq)) AS KEYQT"
    )
}

/// The `SETTINGS` clause both fold queries carry. A query's own clause
/// wins over the session and over the server profile, so nothing outside
/// the query decides how the fold evaluates or how large a query text the
/// server accepts.
///
/// `short_circuit_function_evaluation = 'disable'` makes ClickHouse
/// evaluate every argument of an `if` or a `multiIf` on every row, so an
/// arm runs even on the rows its guard rejects. The step holds under that
/// rule because the only functions in it that fault on data are `intDiv`
/// and `modulo`, and every divisor reaching them is non-zero for any
/// input. An arm added later that can fault on data needs the same
/// treatment.
const FOLD_SETTINGS: &str = "SETTINGS max_threads = 1,\n         \
                             max_ast_elements = 500000, max_expanded_ast_elements = 500000,\n         \
                             max_query_size = 2000000,\n         \
                             short_circuit_function_evaluation = 'disable'";

/// RAM's write-log seed, empty at the start of every real batch.
pub const WL0_EMPTY: &str = "tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64())";

fn init_acc(pc0: &str, regs0: &str, wl0: &str, icount0: &str, keyq0: &str) -> String {
    format!(
        "tuple(toUInt32({pc0}), {regs0}, {wl0}, tuple(toUInt8(0), toUInt8(0), toUInt8(0), toUInt32(0), toUInt32(0)), toUInt64({icount0}), tuple(emptyArrayUInt8(), toUInt32({keyq0}), tuple(toUInt32(0), toUInt8(0))), tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64(), emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64()), tuple(emptyArrayUInt64(), emptyArrayUInt32(), emptyArrayUInt32()))"
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
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    hwm: u32,
    args: &SelectOnlyArgs,
) -> String {
    select_only_variant(
        k,
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        hwm,
        args,
        Variant::Baseline,
    )
}

/// [`select_only`] over the step formulation `variant` names.
#[allow(clippy::too_many_arguments)]
pub fn select_only_variant(
    k: u32,
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    hwm: u32,
    args: &SelectOnlyArgs,
    variant: Variant,
) -> String {
    let step = build_step_variant(
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        RAM_BASE,
        hwm,
        args.ipms,
        variant,
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
         r.7.4 AS pal_wl_addr, r.7.5 AS pal_wl_val, r.7.6 AS pal_wl_icount,\n       \
         r.8.1 AS cp_icount, r.8.2 AS cp_pc, r.8.3 AS cp_regs\n\
         FROM (SELECT arrayFold((acc, i) -> {step}, range({k}), {init}) AS r)\n\
         {FOLD_SETTINGS}",
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
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    hwm: u32,
    args: &BatchArgs,
) -> String {
    batch_variant(
        k,
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        hwm,
        args,
        Variant::Baseline,
    )
}

/// [`batch`] over the step formulation `variant` names.
#[allow(clippy::too_many_arguments)]
pub fn batch_variant(
    k: u32,
    text_start_widx: Widx,
    text_end_widx: Widx,
    decn: u32,
    ram_words: u32,
    hwm: u32,
    args: &BatchArgs,
    variant: Variant,
) -> String {
    let db = args.db;
    let step = build_step_variant(
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        RAM_BASE,
        hwm,
        args.ipms,
        variant,
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
         console_bytes, cp_icount, cp_pc, cp_regs)\n\
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
         r.6.1 AS console_bytes,\n       \
         r.8.1 AS cp_icount, r.8.2 AS cp_pc, r.8.3 AS cp_regs\n\
         FROM (SELECT arrayFold((acc, i) -> {step}, range({k}), {init}) AS r)\n\
         {FOLD_SETTINGS}",
        decode_with(db)
    )
}
