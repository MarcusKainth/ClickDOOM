//! Instruction words to a decoded form, and nothing else.
//!
//! Decoding is separated from execution so the disassembler and the decode
//! cache can both use it, and so a decoder mistake shows up as a wrong
//! mnemonic rather than a wrong result. It computes nothing: an operand read,
//! an address, a branch outcome are all execution's job.
//!
//! An encoding the machine does not implement decodes to `Illegal` rather than
//! failing here, so the halt carries the pc the fetch came from.

/// Every arm the machine has. `Csr` covers all six CSR forms, which stop the
/// machine the same way.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum Op {
    Lui,
    Auipc,
    Jal,
    Jalr,
    Beq,
    Bne,
    Blt,
    Bge,
    Bltu,
    Bgeu,
    Lb,
    Lh,
    Lw,
    Lbu,
    Lhu,
    Sb,
    Sh,
    Sw,
    Addi,
    Slti,
    Sltiu,
    Xori,
    Ori,
    Andi,
    Slli,
    Srli,
    Srai,
    Add,
    Sub,
    Sll,
    Slt,
    Sltu,
    Xor,
    Srl,
    Sra,
    Or,
    And,
    Mul,
    Mulh,
    Mulhsu,
    Mulhu,
    Div,
    Divu,
    Rem,
    Remu,
    Fence,
    Ecall,
    Ebreak,
    Csr,
    Illegal,
}

impl Op {
    /// The mnemonic, as an instruction-mix report names it.
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Op::Lui => "lui",
            Op::Auipc => "auipc",
            Op::Jal => "jal",
            Op::Jalr => "jalr",
            Op::Beq => "beq",
            Op::Bne => "bne",
            Op::Blt => "blt",
            Op::Bge => "bge",
            Op::Bltu => "bltu",
            Op::Bgeu => "bgeu",
            Op::Lb => "lb",
            Op::Lh => "lh",
            Op::Lw => "lw",
            Op::Lbu => "lbu",
            Op::Lhu => "lhu",
            Op::Sb => "sb",
            Op::Sh => "sh",
            Op::Sw => "sw",
            Op::Addi => "addi",
            Op::Slti => "slti",
            Op::Sltiu => "sltiu",
            Op::Xori => "xori",
            Op::Ori => "ori",
            Op::Andi => "andi",
            Op::Slli => "slli",
            Op::Srli => "srli",
            Op::Srai => "srai",
            Op::Add => "add",
            Op::Sub => "sub",
            Op::Sll => "sll",
            Op::Slt => "slt",
            Op::Sltu => "sltu",
            Op::Xor => "xor",
            Op::Srl => "srl",
            Op::Sra => "sra",
            Op::Or => "or",
            Op::And => "and",
            Op::Mul => "mul",
            Op::Mulh => "mulh",
            Op::Mulhsu => "mulhsu",
            Op::Mulhu => "mulhu",
            Op::Div => "div",
            Op::Divu => "divu",
            Op::Rem => "rem",
            Op::Remu => "remu",
            Op::Fence => "fence",
            Op::Ecall => "ecall",
            Op::Ebreak => "ebreak",
            Op::Csr => "csr",
            Op::Illegal => "illegal",
        }
    }
}

/// One decoded instruction. Eight bytes, so a cache over a text region costs
/// two words per instruction.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Instruction {
    pub op: Op,
    pub rd: u8,
    pub rs1: u8,
    pub rs2: u8,
    /// The instruction's immediate, already sign-extended, in the format its
    /// arm uses. Zero where the arm has none.
    pub imm: i32,
}

impl Instruction {
    /// A readable form, for a disassembly listing.
    pub fn render(&self) -> String {
        use Op::*;
        let (rd, rs1, rs2, imm) = (self.rd, self.rs1, self.rs2, self.imm);
        let name = self.op.mnemonic();
        match self.op {
            Lui | Auipc => format!("{name} x{rd}, {:#x}", imm as u32),
            Jal => format!("{name} x{rd}, {imm}"),
            Jalr => format!("{name} x{rd}, {imm}(x{rs1})"),
            Beq | Bne | Blt | Bge | Bltu | Bgeu => format!("{name} x{rs1}, x{rs2}, {imm}"),
            Lb | Lh | Lw | Lbu | Lhu => format!("{name} x{rd}, {imm}(x{rs1})"),
            Sb | Sh | Sw => format!("{name} x{rs2}, {imm}(x{rs1})"),
            Slli | Srli | Srai => format!("{name} x{rd}, x{rs1}, {}", imm & 0x1F),
            Addi | Slti | Sltiu | Xori | Ori | Andi => format!("{name} x{rd}, x{rs1}, {imm}"),
            Add | Sub | Sll | Slt | Sltu | Xor | Srl | Sra | Or | And | Mul | Mulh | Mulhsu
            | Mulhu | Div | Divu | Rem | Remu => format!("{name} x{rd}, x{rs1}, x{rs2}"),
            Csr => format!("{name} x{rd}, x{rs1}, {:#x}", imm & 0xFFF),
            Fence | Ecall | Ebreak | Illegal => name.to_owned(),
        }
    }

    const fn illegal() -> Self {
        Self {
            op: Op::Illegal,
            rd: 0,
            rs1: 0,
            rs2: 0,
            imm: 0,
        }
    }
}

/// Sign-extends the low `bits` of `value`.
const fn sext(value: u32, bits: u32) -> i32 {
    ((value << (32 - bits)) as i32) >> (32 - bits)
}

const OP_LUI: u32 = 0x37;
const OP_AUIPC: u32 = 0x17;
const OP_JAL: u32 = 0x6F;
const OP_JALR: u32 = 0x67;
const OP_BRANCH: u32 = 0x63;
const OP_LOAD: u32 = 0x03;
const OP_STORE: u32 = 0x23;
const OP_IMM: u32 = 0x13;
const OP_REG: u32 = 0x33;
const OP_FENCE: u32 = 0x0F;
const OP_SYSTEM: u32 = 0x73;

pub const fn decode(word: u32) -> Instruction {
    let opcode = word & 0x7F;
    let rd = ((word >> 7) & 0x1F) as u8;
    let funct3 = (word >> 12) & 0x7;
    let rs1 = ((word >> 15) & 0x1F) as u8;
    let rs2 = ((word >> 20) & 0x1F) as u8;
    let funct7 = (word >> 25) & 0x7F;

    let i_imm = sext(word >> 20, 12);

    macro_rules! insn {
        ($op:expr, $imm:expr) => {
            Instruction {
                op: $op,
                rd,
                rs1,
                rs2,
                imm: $imm,
            }
        };
    }

    match opcode {
        OP_LUI => insn!(Op::Lui, (word & 0xFFFF_F000) as i32),
        OP_AUIPC => insn!(Op::Auipc, (word & 0xFFFF_F000) as i32),
        OP_JAL => {
            let imm = sext(
                (((word >> 31) & 0x1) << 20)
                    | (((word >> 12) & 0xFF) << 12)
                    | (((word >> 20) & 0x1) << 11)
                    | (((word >> 21) & 0x3FF) << 1),
                21,
            );
            insn!(Op::Jal, imm)
        }
        OP_JALR => {
            if funct3 != 0b000 {
                return Instruction::illegal();
            }
            insn!(Op::Jalr, i_imm)
        }
        OP_BRANCH => {
            let op = match funct3 {
                0b000 => Op::Beq,
                0b001 => Op::Bne,
                0b100 => Op::Blt,
                0b101 => Op::Bge,
                0b110 => Op::Bltu,
                0b111 => Op::Bgeu,
                _ => return Instruction::illegal(),
            };
            let imm = sext(
                (((word >> 31) & 0x1) << 12)
                    | (((word >> 7) & 0x1) << 11)
                    | (((word >> 25) & 0x3F) << 5)
                    | (((word >> 8) & 0xF) << 1),
                13,
            );
            insn!(op, imm)
        }
        OP_LOAD => {
            let op = match funct3 {
                0b000 => Op::Lb,
                0b001 => Op::Lh,
                0b010 => Op::Lw,
                0b100 => Op::Lbu,
                0b101 => Op::Lhu,
                _ => return Instruction::illegal(),
            };
            insn!(op, i_imm)
        }
        OP_STORE => {
            let op = match funct3 {
                0b000 => Op::Sb,
                0b001 => Op::Sh,
                0b010 => Op::Sw,
                _ => return Instruction::illegal(),
            };
            let imm = sext((((word >> 25) & 0x7F) << 5) | ((word >> 7) & 0x1F), 12);
            insn!(op, imm)
        }
        OP_IMM => {
            let op = match (funct3, funct7) {
                (0b000, _) => Op::Addi,
                (0b010, _) => Op::Slti,
                (0b011, _) => Op::Sltiu,
                (0b100, _) => Op::Xori,
                (0b110, _) => Op::Ori,
                (0b111, _) => Op::Andi,
                (0b001, 0x00) => Op::Slli,
                (0b101, 0x00) => Op::Srli,
                (0b101, 0x20) => Op::Srai,
                _ => return Instruction::illegal(),
            };
            insn!(op, i_imm)
        }
        OP_REG => {
            // The multiply extension shares this opcode, selected by its
            // own function-seven field, and every function-three value under
            // it names an arm.
            if funct7 == 0x01 {
                let op = match funct3 {
                    0b000 => Op::Mul,
                    0b001 => Op::Mulh,
                    0b010 => Op::Mulhsu,
                    0b011 => Op::Mulhu,
                    0b100 => Op::Div,
                    0b101 => Op::Divu,
                    0b110 => Op::Rem,
                    _ => Op::Remu,
                };
                return insn!(op, 0);
            }
            let op = match (funct3, funct7) {
                (0b000, 0x00) => Op::Add,
                (0b000, 0x20) => Op::Sub,
                (0b001, 0x00) => Op::Sll,
                (0b010, 0x00) => Op::Slt,
                (0b011, 0x00) => Op::Sltu,
                (0b100, 0x00) => Op::Xor,
                (0b101, 0x00) => Op::Srl,
                (0b101, 0x20) => Op::Sra,
                (0b110, 0x00) => Op::Or,
                (0b111, 0x00) => Op::And,
                _ => return Instruction::illegal(),
            };
            insn!(op, 0)
        }
        // Every funct3 is a retiring no-op. A single hart with no cache has
        // nothing to reorder against, and the toolchain emits these.
        OP_FENCE => insn!(Op::Fence, 0),
        OP_SYSTEM => {
            if funct3 == 0b000 {
                return match word >> 20 {
                    0 => insn!(Op::Ecall, 0),
                    1 => insn!(Op::Ebreak, 0),
                    _ => Instruction::illegal(),
                };
            }
            match funct3 {
                0b001 | 0b010 | 0b011 | 0b101 | 0b110 | 0b111 => insn!(Op::Csr, i_imm),
                _ => Instruction::illegal(),
            }
        }
        _ => Instruction::illegal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::*;

    fn op(word: u32) -> Op {
        decode(word).op
    }

    #[test]
    fn every_encoder_decodes_back_to_its_own_arm() {
        let cases: &[(u32, Op)] = &[
            (lui(1, 1), Op::Lui),
            (auipc(1, 1), Op::Auipc),
            (jal(1, 4), Op::Jal),
            (jalr(1, 2, 4), Op::Jalr),
            (beq(1, 2, 4), Op::Beq),
            (bne(1, 2, 4), Op::Bne),
            (blt(1, 2, 4), Op::Blt),
            (bge(1, 2, 4), Op::Bge),
            (bltu(1, 2, 4), Op::Bltu),
            (bgeu(1, 2, 4), Op::Bgeu),
            (lb(1, 2, 4), Op::Lb),
            (lh(1, 2, 4), Op::Lh),
            (lw(1, 2, 4), Op::Lw),
            (lbu(1, 2, 4), Op::Lbu),
            (lhu(1, 2, 4), Op::Lhu),
            (sb(1, 2, 4), Op::Sb),
            (sh(1, 2, 4), Op::Sh),
            (sw(1, 2, 4), Op::Sw),
            (addi(1, 2, 4), Op::Addi),
            (slti(1, 2, 4), Op::Slti),
            (sltiu(1, 2, 4), Op::Sltiu),
            (xori(1, 2, 4), Op::Xori),
            (ori(1, 2, 4), Op::Ori),
            (andi(1, 2, 4), Op::Andi),
            (slli(1, 2, 4), Op::Slli),
            (srli(1, 2, 4), Op::Srli),
            (srai(1, 2, 4), Op::Srai),
            (add(1, 2, 3), Op::Add),
            (sub(1, 2, 3), Op::Sub),
            (sll(1, 2, 3), Op::Sll),
            (slt(1, 2, 3), Op::Slt),
            (sltu(1, 2, 3), Op::Sltu),
            (xor(1, 2, 3), Op::Xor),
            (srl(1, 2, 3), Op::Srl),
            (sra(1, 2, 3), Op::Sra),
            (or(1, 2, 3), Op::Or),
            (and(1, 2, 3), Op::And),
            (mul(1, 2, 3), Op::Mul),
            (mulh(1, 2, 3), Op::Mulh),
            (mulhsu(1, 2, 3), Op::Mulhsu),
            (mulhu(1, 2, 3), Op::Mulhu),
            (div(1, 2, 3), Op::Div),
            (divu(1, 2, 3), Op::Divu),
            (rem(1, 2, 3), Op::Rem),
            (remu(1, 2, 3), Op::Remu),
            (fence(), Op::Fence),
            (ecall(), Op::Ecall),
            (ebreak(), Op::Ebreak),
            (csrrw(1, 2, 0x340), Op::Csr),
            (RESERVED_OPCODE, Op::Illegal),
        ];
        for (word, expected) in cases {
            assert_eq!(op(*word), *expected, "word {word:#010x}");
        }
    }

    #[test]
    fn the_register_fields_come_out_where_they_went_in() {
        let insn = decode(add(31, 30, 29));
        assert_eq!((insn.rd, insn.rs1, insn.rs2), (31, 30, 29));
    }

    #[test]
    fn immediates_come_out_sign_extended() {
        assert_eq!(decode(addi(1, 2, -1)).imm, -1);
        assert_eq!(decode(addi(1, 2, 2047)).imm, 2047);
        assert_eq!(decode(addi(1, 2, -2048)).imm, -2048);
        assert_eq!(decode(sw(1, 2, -4)).imm, -4);
        assert_eq!(decode(beq(1, 2, -4096)).imm, -4096);
        assert_eq!(decode(jal(1, -1048576)).imm, -1048576);
        assert_eq!(decode(lui(1, 0xFFFFF)).imm, 0xFFFF_F000u32 as i32);
    }

    #[test]
    fn a_shift_immediate_carries_its_amount_in_the_low_five_bits() {
        assert_eq!(decode(slli(1, 2, 31)).imm & 0x1F, 31);
        assert_eq!(decode(srli(1, 2, 7)).imm & 0x1F, 7);
        assert_eq!(decode(srai(1, 2, 7)).imm & 0x1F, 7);
    }

    #[test]
    fn the_holes_in_each_arm_are_illegal() {
        // Loads and stores have no arm for these function codes.
        for funct3 in [0b011, 0b110, 0b111] {
            assert_eq!(op(i_type(0x03, 1, funct3, 2, 0)), Op::Illegal);
        }
        for funct3 in [0b011, 0b100, 0b101, 0b110, 0b111] {
            assert_eq!(op(s_type(0x23, funct3, 1, 2, 0)), Op::Illegal);
        }
        // Branches have none for 010 and 011.
        for funct3 in [0b010, 0b011] {
            assert_eq!(op(b_type(0x63, funct3, 1, 2, 4)), Op::Illegal);
        }
        // An indirect jump is only defined for function code zero.
        for funct3 in 1..8 {
            assert_eq!(op(i_type(0x67, 1, funct3, 2, 0)), Op::Illegal);
        }
        // A shift immediate with a function-seven field that names no arm.
        assert_eq!(op(r_type(0x13, 1, 0b001, 2, 4, 0x20)), Op::Illegal);
        assert_eq!(op(r_type(0x13, 1, 0b101, 2, 4, 0x10)), Op::Illegal);
        // The system arm has no function code four, and no immediate beyond
        // the two it names.
        assert_eq!(op(i_type(0x73, 0, 0b100, 0, 0)), Op::Illegal);
        assert_eq!(op(i_type(0x73, 0, 0b000, 0, 2)), Op::Illegal);
    }

    #[test]
    fn every_multiply_function_code_names_an_arm() {
        let arms = [
            Op::Mul,
            Op::Mulh,
            Op::Mulhsu,
            Op::Mulhu,
            Op::Div,
            Op::Divu,
            Op::Rem,
            Op::Remu,
        ];
        for (funct3, expected) in arms.iter().enumerate() {
            assert_eq!(op(r_type(0x33, 1, funct3 as u32, 2, 3, 0x01)), *expected);
        }
    }

    #[test]
    fn every_opcode_whose_low_bits_are_not_set_is_illegal() {
        // No RV32IM instruction has anything but 11 in its low two bits.
        for opcode in (0u32..128).filter(|o| o & 0b11 != 0b11) {
            assert_eq!(op(opcode), Op::Illegal, "opcode {opcode:#04x}");
        }
    }

    #[test]
    fn a_fence_of_any_function_code_is_a_fence() {
        for funct3 in 0..8 {
            assert_eq!(op(i_type(0x0F, 0, funct3, 0, 0)), Op::Fence);
        }
    }

    #[test]
    fn every_csr_form_lands_on_one_arm() {
        for funct3 in [0b001, 0b010, 0b011, 0b101, 0b110, 0b111] {
            assert_eq!(op(i_type(0x73, 1, funct3, 2, 0x340)), Op::Csr);
        }
    }

    #[test]
    fn a_rendering_names_the_operands_its_format_has() {
        assert_eq!(decode(addi(1, 2, -3)).render(), "addi x1, x2, -3");
        assert_eq!(decode(lw(1, 2, 64)).render(), "lw x1, 64(x2)");
        assert_eq!(decode(sw(2, 1, -4)).render(), "sw x1, -4(x2)");
        assert_eq!(decode(beq(1, 2, 32)).render(), "beq x1, x2, 32");
        assert_eq!(decode(add(1, 2, 3)).render(), "add x1, x2, x3");
        assert_eq!(decode(slli(1, 2, 4)).render(), "slli x1, x2, 4");
        assert_eq!(decode(lui(1, 0xABCDE)).render(), "lui x1, 0xabcde000");
        assert_eq!(decode(ecall()).render(), "ecall");
        assert_eq!(decode(RESERVED_OPCODE).render(), "illegal");
    }

    #[test]
    fn a_decoded_instruction_is_eight_bytes() {
        assert_eq!(std::mem::size_of::<Instruction>(), 8);
    }

    #[test]
    fn no_two_arms_share_a_mnemonic() {
        let mut seen = std::collections::HashSet::new();
        for word in 0u32..=0xFFFF {
            let name = decode(word.wrapping_mul(0x9E37_79B9)).op.mnemonic();
            seen.insert(name);
        }
        assert!(
            seen.len() > 20,
            "the sample reached only {} arms",
            seen.len()
        );
    }
}
