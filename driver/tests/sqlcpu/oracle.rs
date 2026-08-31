//! An independent RV32I reference for one instruction.
//!
//! Written from the instruction set and the decoded-row conventions
//! `sqlcpu/schema.sql` documents, sharing no code with
//! `clickdoom_executor::fold`, so a mistake in one is unlikely to be
//! mirrored in the other.
//!
//! It answers what one step leaves behind: the next pc, the register file,
//! RAM's write-log, and the halt. An instruction that halts does not
//! retire, so it advances neither pc nor a register and stores nothing.
//!
//! `regs` is x1..x31, index 0 holding x1, matching the register file the
//! fold carries. x0 is never stored: a read of register 0 is the constant
//! 0 and a write to it is discarded.

/// The decode ids this reference names. The full numbering is documented
/// above `CREATE TABLE clickdoom.decoded` in `sqlcpu/schema.sql`.
const LOAD: u8 = 18;
const STORE: u8 = 19;
const JAL: u8 = 26;
const JALR: u8 = 27;

/// One decoded instruction plus the machine state it runs against.
pub struct Input<'a> {
    pub pc: u32,
    pub id: u8,
    pub rd: u8,
    pub rs1: u8,
    pub rs2: u8,
    pub imm: u32,
    pub tgt: u32,
    pub mk: u32,
    pub sg: u8,
    pub regs: &'a [u32; 31],
    /// The RAM word the instruction's address lands in, for a load's
    /// extraction and a store's read-modify-write.
    pub mem_word: u32,
    /// RAM's base address. The write-log is keyed on the word index
    /// relative to it.
    pub ram_base: u32,
}

/// What one step leaves behind.
#[derive(PartialEq, Eq, Debug)]
pub struct Step {
    pub pc: u32,
    pub regs: [u32; 31],
    pub wl_addr: Vec<u32>,
    pub wl_val: Vec<u32>,
    pub halted: u8,
    pub halt_reason: String,
    pub retired: u32,
}

fn read(regs: &[u32; 31], r: u8) -> u32 {
    if r == 0 { 0 } else { regs[r as usize - 1] }
}

fn div(a: u32, b: u32) -> u32 {
    if b == 0 {
        u32::MAX
    } else if a as i32 == i32::MIN && b as i32 == -1 {
        i32::MIN as u32
    } else {
        ((a as i32) / (b as i32)) as u32
    }
}

fn rem(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else if a as i32 == i32::MIN && b as i32 == -1 {
        0
    } else {
        ((a as i32) % (b as i32)) as u32
    }
}

/// The byte extracted from `mem_word` at `addr`, widened to a word:
/// `mk` selects the width, `sg` says whether the result is sign-extended.
fn load_value(mem_word: u32, addr: u32, mk: u32, sg: u8) -> u32 {
    let shift = 8 * (addr & 3);
    let extracted = (mem_word >> shift) & mk;
    let sign_bit = (mk >> 1) + 1;
    if sg != 0 && extracted & sign_bit != 0 {
        extracted.wrapping_sub(mk).wrapping_sub(1)
    } else {
        extracted
    }
}

/// `mem_word` with the low `mk` bits of `value` spliced in at `addr`'s byte
/// offset, every byte outside the mask kept.
fn store_value(mem_word: u32, addr: u32, mk: u32, value: u32) -> u32 {
    let shift = 8 * (addr & 3);
    (mem_word & !(mk << shift)) | ((value & mk) << shift)
}

fn alu(input: &Input, a: u32, b: u32, addr: u32) -> u32 {
    let (sa, sb) = (a as i32, b as i32);
    match input.id {
        0 => a.wrapping_add(b),
        1 => a.wrapping_sub(b),
        2 => a << (b & 31),
        3 => u32::from(sa < sb),
        4 => u32::from(a < b),
        5 => a ^ b,
        6 => a >> (b & 31),
        7 => (sa >> (b & 31)) as u32,
        8 => a | b,
        9 => a & b,
        10 => a.wrapping_mul(b),
        11 => (((sa as i64) * (sb as i64)) >> 32) as u32,
        // mulhsu multiplies a signed rs1 by an UNSIGNED rs2, so b widens
        // by zero-extension where a widens by sign-extension.
        12 => (((sa as i64) * (b as i64)) >> 32) as u32,
        13 => (((a as u64) * (b as u64)) >> 32) as u32,
        14 => div(a, b),
        // Unsigned division by zero returns all-ones and unsigned
        // remainder by zero returns the dividend, neither trapping. There
        // is no unsigned counterpart to the signed overflow above.
        15 => a.checked_div(b).unwrap_or(u32::MAX),
        16 => rem(a, b),
        17 => a.checked_rem(b).unwrap_or(a),
        LOAD => load_value(input.mem_word, addr, input.mk, input.sg),
        // jal and jalr link to the following instruction; every remaining
        // id writes no register, so the value never reaches one.
        _ => input.pc.wrapping_add(4),
    }
}

pub fn step(input: &Input) -> Step {
    let a = read(input.regs, input.rs1);
    // The raw rs2 value, which is what a store writes. The ALU's second
    // operand folds the immediate in; a store's immediate is its address
    // offset and is not part of the value stored.
    let rs2_value = read(input.regs, input.rs2);
    let b = rs2_value.wrapping_add(input.imm);
    let addr = a.wrapping_add(input.imm);

    let jalr_target = a.wrapping_add(input.imm) & 0xFFFF_FFFE;
    let taken = match input.id {
        20 => a == b,
        21 => a != b,
        22 => (a as i32) < (b as i32),
        23 => (a as i32) >= (b as i32),
        24 => a < b,
        25 => a >= b,
        JAL | JALR => true,
        _ => false,
    };
    let target = if input.id == JALR {
        jalr_target
    } else {
        input.tgt
    };
    // A branch, jal or jalr whose target is not 4-byte aligned faults on
    // the instruction that computes the target, and the target itself
    // never runs. A branch that is not taken never evaluates its own
    // target's alignment.
    let jump_misaligned = taken && target & 3 != 0;

    let next = match input.id {
        20..=25 => {
            if taken {
                input.tgt
            } else {
                input.pc.wrapping_add(4)
            }
        }
        JAL => input.tgt,
        JALR => jalr_target,
        _ => input.pc.wrapping_add(4),
    };

    let halt_reason = match input.id {
        28 => "ECALL",
        29 => "EBREAK",
        30 => "CSR",
        31 => "ILLEGAL_INSN",
        _ if jump_misaligned => "MISALIGNED",
        _ => "",
    };
    let halted = u8::from(!halt_reason.is_empty());
    let retires = halted == 0;

    let mut regs = *input.regs;
    if retires && input.rd != 0 {
        regs[input.rd as usize - 1] = alu(input, a, b, addr);
    }

    let (mut wl_addr, mut wl_val) = (Vec::new(), Vec::new());
    if retires && input.id == STORE {
        wl_addr.push((addr.wrapping_sub(input.ram_base)) >> 2);
        wl_val.push(store_value(input.mem_word, addr, input.mk, rs2_value));
    }

    Step {
        pc: if retires { next } else { input.pc },
        regs,
        wl_addr,
        wl_val,
        halted,
        halt_reason: halt_reason.to_owned(),
        retired: u32::from(retires),
    }
}
