//! Builders for the decode-table rows the tests hand-write.
//!
//! Every address-carrying builder puts the whole address in the immediate
//! and leaves rs1 as x0. x0 reads as 0 and does not hold RAM_BASE, so a
//! store or load written this way addresses exactly what its argument says.

use super::reference::{Insn, OP_LOAD, OP_STORE};

/// Load and store width masks.
pub const WORD: u32 = 0xFFFF_FFFF;
pub const HALF: u32 = 0xFFFF;
pub const BYTE: u32 = 0xFF;

/// `addi rd, x0, imm`. Both sources are x0, so the immediate is the whole
/// result.
pub fn addi(rd: u8, imm: u32) -> Insn {
    Insn {
        op_id: 0,
        rd,
        imm,
        ..Insn::default()
    }
}

/// A two-source ALU arm writing rd.
pub fn alu(op_id: u32, rd: u8, rs1: u8, rs2: u8) -> Insn {
    Insn {
        op_id,
        rd,
        rs1,
        rs2,
        ..Insn::default()
    }
}

/// A store of x\[rs2\] at an absolute address.
pub fn store(rs2: u8, addr: u32, width_mask: u32) -> Insn {
    Insn {
        op_id: OP_STORE,
        rs2,
        imm: addr,
        width_mask,
        ..Insn::default()
    }
}

/// A load into rd from an absolute address.
pub fn load(rd: u8, addr: u32, width_mask: u32, sign_bit: u8) -> Insn {
    Insn {
        op_id: OP_LOAD,
        rd,
        imm: addr,
        width_mask,
        sign_bit,
        ..Insn::default()
    }
}

/// A branch comparing x\[rs1\] against x\[rs2\], to a byte address.
pub fn branch(op_id: u32, rs1: u8, rs2: u8, target: u32) -> Insn {
    Insn {
        op_id,
        rs1,
        rs2,
        target,
        ..Insn::default()
    }
}

/// `jal rd, target`, to a byte address.
pub fn jal(rd: u8, target: u32) -> Insn {
    Insn {
        op_id: 26,
        rd,
        target,
        ..Insn::default()
    }
}

/// An instruction whose op_id alone decides what happens: the fatal-halt
/// decode arms.
pub fn bare(op_id: u32) -> Insn {
    Insn {
        op_id,
        ..Insn::default()
    }
}
