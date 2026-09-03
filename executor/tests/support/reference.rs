//! An independent interpreter over the same collapsed op_id representation
//! the fold implements.
//!
//! Not an RV32IM interpreter and no substitute for `refemu`. It answers one
//! narrower question: given a stream of already-decoded rows, does the fold
//! implement the collapsed semantics correctly? Whether a real `addi`
//! decodes to the right op_id and immediate is a decode question, settled
//! by the differential run against `refemu`.
//!
//! Nothing here reads `clickdoom_executor`. The arithmetic and the halt
//! codes are written from the semantics, so a mistake in the fold and a
//! mistake here cannot cancel out. The halt-reason precedence is part of
//! those semantics: misalignment is decided before the address's region.
//!
//! The regions outside RAM are deliberately unmodelled: MMIO, FRAMEBUFFER
//! and PALETTE all read here as an address outside RAM, so a case touching
//! any of them has no expected value to compare against and asserts on the
//! fold's own output instead.

/// Halt codes, matching the fold accumulator's `halt_reason` numbering.
/// `HALT_EXIT` has no entry: a store to the EXIT register is an MMIO
/// access, which this model does not have.
pub const HALT_NONE: u8 = 0;
pub const HALT_ILLEGAL_INSN: u8 = 1;
pub const HALT_SELF_MODIFY: u8 = 2;
pub const HALT_BAD_ADDR: u8 = 3;
pub const HALT_MISALIGNED: u8 = 4;
pub const HALT_ECALL: u8 = 5;
pub const HALT_EBREAK: u8 = 6;
pub const HALT_CSR: u8 = 7;

pub const OP_LOAD: u32 = 18;
pub const OP_STORE: u32 = 19;
pub const OP_ECALL: u32 = 28;
pub const OP_EBREAK: u32 = 29;
pub const OP_CSR: u32 = 30;
pub const OP_ILLEGAL: u32 = 31;

/// One row of the decode table. `imm` and `target` hold the value the
/// `decoded` table stores, so a negative immediate is its two's-complement
/// bit pattern.
#[derive(Clone, Copy, Debug)]
pub struct Insn {
    pub op_id: u32,
    pub rd: u8,
    pub rs1: u8,
    pub rs2: u8,
    pub imm: u32,
    pub target: u32,
    pub width_mask: u32,
    pub sign_bit: u8,
    pub raw: u32,
}

impl Default for Insn {
    fn default() -> Self {
        Insn {
            op_id: 0,
            rd: 0,
            rs1: 0,
            rs2: 0,
            imm: 0,
            target: 0,
            width_mask: 0xFFFF_FFFF,
            sign_bit: 0,
            raw: 0,
        }
    }
}

/// What one run of the model leaves behind, in the fold accumulator's own
/// field names. `regs` is x1..x31, with no slot for x0, matching the shape
/// the fold projects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub pc: u32,
    pub regs: [u32; 31],
    pub wl_addr: Vec<u32>,
    pub wl_val: Vec<u32>,
    pub wl_icount: Vec<u64>,
    pub stopped: u8,
    pub halted: u8,
    pub halt_reason: u8,
    pub halt_pc: u32,
    pub halt_extra: u32,
    pub retired: u32,
}

/// One run's inputs. `insns` is the decode table, indexed by word offset
/// from `ram_base`; `ram0` is the memory the run starts from, dense over
/// `[0, ram_words)` and word-indexed relative to `ram_base`.
pub struct Run<'a> {
    pub insns: &'a [Insn],
    pub ram_base: u32,
    pub ram_words: u32,
    pub text_start_widx: u32,
    pub text_end_widx: u32,
    pub ram0: &'a [u32],
    pub regs0: [u32; 31],
    pub pc0: u32,
    pub k: u32,
    pub hwm: u32,
}

/// Executes at most `k` steps. `pc` is a byte address throughout, matching
/// the fold's accumulator and the reset value: a jump target with bit 1 set
/// stays representable, which is the bit the misaligned-target check reads.
///
/// `ram_words` has to be a power of two, since the word index is masked
/// rather than clamped into the RAM window.
pub fn run(args: &Run) -> Outcome {
    assert!(
        args.ram_words.is_power_of_two(),
        "ram_words={} must be a power of two: the word index is masked into the window",
        args.ram_words
    );
    assert!(!args.insns.is_empty(), "the decode table must have a row");

    let mut regs = [0u32; 32];
    regs[1..].copy_from_slice(&args.regs0);

    let mut wl_addr: Vec<u32> = Vec::new();
    let mut wl_val: Vec<u32> = Vec::new();
    let mut wl_icount: Vec<u64> = Vec::new();

    let mut pc = args.pc0;
    let mut stopped = false;
    let mut halted = false;
    let mut halt_reason = HALT_NONE;
    let mut halt_pc = 0u32;
    let mut halt_extra = 0u32;
    let mut retired: u32 = 0;

    let ram_end = args.ram_base as u64 + args.ram_words as u64 * 4;

    for _ in 0..args.k {
        if stopped {
            break;
        }
        // The text region is the only part of RAM a fetch may come from. An
        // empty window declares no region, which is how a caller that wants
        // every word fetchable spells it.
        let pc_widx = pc.wrapping_sub(args.ram_base) >> 2;
        if args.text_end_widx != args.text_start_widx
            && !(args.text_start_widx..args.text_end_widx).contains(&pc_widx)
        {
            halted = true;
            stopped = true;
            halt_pc = pc;
            halt_reason = HALT_BAD_ADDR;
            halt_extra = pc;
            break;
        }
        let ins = args.insns[decode_idx(pc, args.ram_base, args.insns.len())];
        let a = regs[ins.rs1 as usize];
        let b = regs[ins.rs2 as usize].wrapping_add(ins.imm);
        let (sa, sb) = (a as i32, b as i32);
        let addr = a.wrapping_add(ins.imm);
        let is_mem = ins.op_id == OP_LOAD || ins.op_id == OP_STORE;
        let align_mask: u32 = match ins.width_mask {
            0xFFFF_FFFF => 3,
            0xFFFF => 1,
            _ => 0,
        };
        // Misalignment is decided before the region, so an address that is
        // both misaligned and outside every region is misaligned.
        let misaligned = is_mem && (addr & align_mask) != 0;
        let bad_addr = is_mem
            && !misaligned
            && !((addr as u64) >= args.ram_base as u64 && (addr as u64) < ram_end);
        let wa = (addr.wrapping_sub(args.ram_base) >> 2) & (args.ram_words - 1);
        let self_modify = ins.op_id == OP_STORE
            && !bad_addr
            && !misaligned
            && wa >= args.text_start_widx
            && wa < args.text_end_widx;
        let decode_fatal = matches!(ins.op_id, OP_ECALL | OP_EBREAK | OP_CSR | OP_ILLEGAL);

        // A misaligned jump or branch target halts at the transferring
        // instruction, and neither pc nor rd updates. jal and jalr always
        // transfer; a branch only when it is taken.
        let (would_jump, jump_target) = would_jump(&ins, a, b, sa, sb);
        let jump_misaligned = would_jump && (jump_target & 3) != 0;

        if decode_fatal || bad_addr || misaligned || self_modify || jump_misaligned {
            halted = true;
            stopped = true;
            halt_pc = pc;
            (halt_reason, halt_extra) = if ins.op_id == OP_ILLEGAL {
                (HALT_ILLEGAL_INSN, ins.raw)
            } else if jump_misaligned {
                (HALT_MISALIGNED, jump_target)
            } else if self_modify {
                (HALT_SELF_MODIFY, addr)
            } else if misaligned {
                (HALT_MISALIGNED, addr)
            } else if bad_addr {
                (HALT_BAD_ADDR, addr)
            } else if ins.op_id == OP_ECALL {
                (HALT_ECALL, 0)
            } else if ins.op_id == OP_EBREAK {
                (HALT_EBREAK, 0)
            } else {
                (HALT_CSR, 0)
            };
            break;
        }

        let sh = 8 * (addr & 3);
        let mut lw = 0u32;
        let result;
        if ins.op_id == OP_LOAD {
            lw = mem_read_word(&wl_addr, &wl_val, args.ram0, wa);
            let mut v = (lw >> sh) & ins.width_mask;
            // `sign_bit` is a flag. The sign bit's position comes from
            // `width_mask`.
            let sign_pos = (ins.width_mask >> 1) + 1;
            if ins.sign_bit != 0 && (v & sign_pos) != 0 {
                v = v.wrapping_sub((ins.width_mask as u64 + 1) as u32);
            }
            result = v;
        } else if ins.op_id == OP_STORE {
            lw = mem_read_word(&wl_addr, &wl_val, args.ram0, wa);
            result = 0;
        } else {
            result = alu(ins.op_id, a, b, sa, sb, pc.wrapping_add(4));
        }

        let next = next_pc(&ins, pc, would_jump, jump_target);

        if ins.op_id == OP_STORE {
            // The stored value is the raw x[rs2], not `b`: the immediate is
            // the address offset, already spent on `addr`.
            let sval =
                (lw & !(ins.width_mask << sh)) | ((regs[ins.rs2 as usize] & ins.width_mask) << sh);
            wl_addr.push(wa);
            wl_val.push(sval);
            wl_icount.push(retired as u64 + 1);
        } else if ins.rd != 0 {
            regs[ins.rd as usize] = result;
        }

        pc = next;
        retired += 1;
        if ins.op_id == OP_STORE && wl_addr.len() as u32 >= args.hwm {
            stopped = true;
        }
    }

    let mut out_regs = [0u32; 31];
    out_regs.copy_from_slice(&regs[1..]);
    Outcome {
        pc,
        regs: out_regs,
        wl_addr,
        wl_val,
        wl_icount,
        stopped: stopped as u8,
        halted: halted as u8,
        halt_reason,
        halt_pc,
        halt_extra,
        retired,
    }
}

/// The write-log's latest value for `widx`, falling back to RAM. Mirrors
/// the fold's `arrayLastIndex` forwarding: the last matching entry wins.
fn mem_read_word(wl_addr: &[u32], wl_val: &[u32], ram: &[u32], widx: u32) -> u32 {
    for i in (0..wl_addr.len()).rev() {
        if wl_addr[i] == widx {
            return wl_val[i];
        }
    }
    ram.get(widx as usize).copied().unwrap_or(0)
}

/// The decode table index for a byte pc, clamped into the table's own
/// range so a lookup is always in bounds. `pc` itself is never rounded.
fn decode_idx(byte_pc: u32, ram_base: u32, len: usize) -> usize {
    let raw = byte_pc.wrapping_sub(ram_base);
    ((raw >> 2) as usize).min(len - 1)
}

/// Whether this instruction transfers control, and the byte address it
/// transfers to. jalr clears bit 0 only, so a target with bit 1 set stays
/// checkable.
fn would_jump(ins: &Insn, a: u32, b: u32, sa: i32, sb: i32) -> (bool, u32) {
    match ins.op_id {
        20 => (a == b, ins.target),
        21 => (a != b, ins.target),
        22 => (sa < sb, ins.target),
        23 => (sa >= sb, ins.target),
        24 => (a < b, ins.target),
        25 => (a >= b, ins.target),
        26 => (true, ins.target),
        27 => (true, a.wrapping_add(ins.imm) & 0xFFFF_FFFE),
        _ => (false, 0),
    }
}

/// `link_value` is the fallback arm, which jal and jalr write to rd. It is
/// pc + 4, never the jump target.
fn alu(op_id: u32, a: u32, b: u32, sa: i32, sb: i32, link_value: u32) -> u32 {
    match op_id {
        0 => a.wrapping_add(b),
        1 => a.wrapping_sub(b),
        2 => a << (b & 31),
        3 => (sa < sb) as u32,
        4 => (a < b) as u32,
        5 => a ^ b,
        6 => a >> (b & 31),
        7 => (sa >> (b & 31)) as u32,
        8 => a | b,
        9 => a & b,
        10 => (sa as i64 * sb as i64) as u32,
        11 => ((sa as i64 * sb as i64) >> 32) as u32,
        // mulhsu: rs1 signed, rs2 unsigned.
        12 => ((sa as i64 * b as i64) >> 32) as u32,
        13 => ((a as u64 * b as u64) >> 32) as u32,
        // Widening to 64 bits is what keeps INT_MIN / -1 from overflowing:
        // the quotient +2**31 does not fit back into 32 signed bits, and
        // truncating the 64-bit result gives the defined RV32IM answer.
        14 => {
            if sb == 0 {
                u32::MAX
            } else {
                (sa as i64 / sb as i64) as u32
            }
        }
        15 => a.checked_div(b).unwrap_or(u32::MAX),
        16 => {
            if sb == 0 {
                a
            } else {
                (sa as i64 % sb as i64) as u32
            }
        }
        17 => a.checked_rem(b).unwrap_or(a),
        _ => link_value,
    }
}

/// Fallthrough is pc + 4, unclamped. A taken jump or branch has already had
/// its target and its alignment checked by the caller.
fn next_pc(ins: &Insn, pc: u32, would_jump: bool, jump_target: u32) -> u32 {
    if (20..=27).contains(&ins.op_id) && would_jump {
        jump_target
    } else {
        pc.wrapping_add(4)
    }
}
