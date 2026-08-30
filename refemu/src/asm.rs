//! Instruction-word encoders.
//!
//! Each one is a transcription of an RV32IM instruction format, so a reader
//! checks an encoding against the ISA manual line by line. Nothing here
//! decodes or executes, which is why the decoder can be tested against it.
//!
//! This is a public module rather than a test helper. Integration tests and
//! the differential fuzzer both build programs with it.

const fn imm_bits(value: i32, bits: u32) -> u32 {
    (value as u32) & ((1u32 << bits) - 1)
}

pub const fn r_type(opcode: u32, rd: u32, funct3: u32, rs1: u32, rs2: u32, funct7: u32) -> u32 {
    (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

pub const fn i_type(opcode: u32, rd: u32, funct3: u32, rs1: u32, imm: i32) -> u32 {
    (imm_bits(imm, 12) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
}

pub const fn s_type(opcode: u32, funct3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
    let imm = imm_bits(imm, 12);
    ((imm >> 5) << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | ((imm & 0x1F) << 7) | opcode
}

/// Branch offsets are even, so bit 0 is not encoded.
pub const fn b_type(opcode: u32, funct3: u32, rs1: u32, rs2: u32, imm: i32) -> u32 {
    let imm = imm_bits(imm, 13);
    ((imm >> 12) & 1) << 31
        | ((imm >> 5) & 0x3F) << 25
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | ((imm >> 1) & 0xF) << 8
        | ((imm >> 11) & 1) << 7
        | opcode
}

pub const fn u_type(opcode: u32, rd: u32, imm20: i32) -> u32 {
    (imm_bits(imm20, 20) << 12) | (rd << 7) | opcode
}

/// Jump offsets are even, so bit 0 is not encoded.
pub const fn j_type(opcode: u32, rd: u32, imm: i32) -> u32 {
    let imm = imm_bits(imm, 21);
    ((imm >> 20) & 1) << 31
        | ((imm >> 1) & 0x3FF) << 21
        | ((imm >> 11) & 1) << 20
        | ((imm >> 12) & 0xFF) << 12
        | (rd << 7)
        | opcode
}

macro_rules! encoders {
    ($($(#[$m:meta])* $name:ident($($arg:ident: $ty:ty),*) = $body:expr;)*) => {
        $($(#[$m])* pub const fn $name($($arg: $ty),*) -> u32 { $body })*
    };
}

encoders! {
    lui(rd: u32, imm20: i32) = u_type(0x37, rd, imm20);
    auipc(rd: u32, imm20: i32) = u_type(0x17, rd, imm20);
    jal(rd: u32, imm: i32) = j_type(0x6F, rd, imm);
    jalr(rd: u32, rs1: u32, imm: i32) = i_type(0x67, rd, 0b000, rs1, imm);

    beq(rs1: u32, rs2: u32, imm: i32) = b_type(0x63, 0b000, rs1, rs2, imm);
    bne(rs1: u32, rs2: u32, imm: i32) = b_type(0x63, 0b001, rs1, rs2, imm);
    blt(rs1: u32, rs2: u32, imm: i32) = b_type(0x63, 0b100, rs1, rs2, imm);
    bge(rs1: u32, rs2: u32, imm: i32) = b_type(0x63, 0b101, rs1, rs2, imm);
    bltu(rs1: u32, rs2: u32, imm: i32) = b_type(0x63, 0b110, rs1, rs2, imm);
    bgeu(rs1: u32, rs2: u32, imm: i32) = b_type(0x63, 0b111, rs1, rs2, imm);

    lb(rd: u32, rs1: u32, imm: i32) = i_type(0x03, rd, 0b000, rs1, imm);
    lh(rd: u32, rs1: u32, imm: i32) = i_type(0x03, rd, 0b001, rs1, imm);
    lw(rd: u32, rs1: u32, imm: i32) = i_type(0x03, rd, 0b010, rs1, imm);
    lbu(rd: u32, rs1: u32, imm: i32) = i_type(0x03, rd, 0b100, rs1, imm);
    lhu(rd: u32, rs1: u32, imm: i32) = i_type(0x03, rd, 0b101, rs1, imm);

    sb(rs1: u32, rs2: u32, imm: i32) = s_type(0x23, 0b000, rs1, rs2, imm);
    sh(rs1: u32, rs2: u32, imm: i32) = s_type(0x23, 0b001, rs1, rs2, imm);
    sw(rs1: u32, rs2: u32, imm: i32) = s_type(0x23, 0b010, rs1, rs2, imm);

    addi(rd: u32, rs1: u32, imm: i32) = i_type(0x13, rd, 0b000, rs1, imm);
    slti(rd: u32, rs1: u32, imm: i32) = i_type(0x13, rd, 0b010, rs1, imm);
    sltiu(rd: u32, rs1: u32, imm: i32) = i_type(0x13, rd, 0b011, rs1, imm);
    xori(rd: u32, rs1: u32, imm: i32) = i_type(0x13, rd, 0b100, rs1, imm);
    ori(rd: u32, rs1: u32, imm: i32) = i_type(0x13, rd, 0b110, rs1, imm);
    andi(rd: u32, rs1: u32, imm: i32) = i_type(0x13, rd, 0b111, rs1, imm);
    slli(rd: u32, rs1: u32, shamt: u32) = r_type(0x13, rd, 0b001, rs1, shamt, 0x00);
    srli(rd: u32, rs1: u32, shamt: u32) = r_type(0x13, rd, 0b101, rs1, shamt, 0x00);
    srai(rd: u32, rs1: u32, shamt: u32) = r_type(0x13, rd, 0b101, rs1, shamt, 0x20);

    add(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b000, rs1, rs2, 0x00);
    sub(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b000, rs1, rs2, 0x20);
    sll(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b001, rs1, rs2, 0x00);
    slt(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b010, rs1, rs2, 0x00);
    sltu(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b011, rs1, rs2, 0x00);
    xor(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b100, rs1, rs2, 0x00);
    srl(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b101, rs1, rs2, 0x00);
    sra(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b101, rs1, rs2, 0x20);
    or(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b110, rs1, rs2, 0x00);
    and(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b111, rs1, rs2, 0x00);

    mul(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b000, rs1, rs2, 0x01);
    mulh(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b001, rs1, rs2, 0x01);
    mulhsu(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b010, rs1, rs2, 0x01);
    mulhu(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b011, rs1, rs2, 0x01);
    div(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b100, rs1, rs2, 0x01);
    divu(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b101, rs1, rs2, 0x01);
    rem(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b110, rs1, rs2, 0x01);
    remu(rd: u32, rs1: u32, rs2: u32) = r_type(0x33, rd, 0b111, rs1, rs2, 0x01);

    fence() = i_type(0x0F, 0, 0b000, 0, 0);
    ecall() = i_type(0x73, 0, 0b000, 0, 0);
    ebreak() = i_type(0x73, 0, 0b000, 0, 1);
    csrrw(rd: u32, rs1: u32, csr: i32) = i_type(0x73, rd, 0b001, rs1, csr);
}

/// Opcode bits all zero, which no RV32IM instruction uses.
pub const RESERVED_OPCODE: u32 = 0;

/// The canonical no-op, which is what a shrinker replaces an instruction
/// with when it is trying to make a failing case smaller.
pub const NOP: u32 = addi(0, 0, 0);

/// The canonical no-op, as a call, for building a program with `vec!`.
pub const fn nop() -> u32 {
    NOP
}

/// Packs words into the bytes a loader takes.
pub fn program(words: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_place_every_field_where_the_manual_says() {
        assert_eq!(addi(1, 2, -1), 0xFFF1_0093);
        assert_eq!(lui(5, 0x12345), 0x1234_52B7);
        assert_eq!(sw(2, 3, 8), 0x0031_2423);
        assert_eq!(add(1, 2, 3), 0x0031_00B3);
        assert_eq!(sub(1, 2, 3), 0x4031_00B3);
        assert_eq!(ecall(), 0x0000_0073);
        assert_eq!(ebreak(), 0x0010_0073);
        assert_eq!(fence(), 0x0000_000F);
    }

    #[test]
    fn a_branch_offset_survives_the_scattered_immediate() {
        // The immediate is split across bits 31, 30:25, 11:8 and 7. Rebuild it
        // and check it comes back.
        for offset in [-4096i32, -2048, -4, 0, 4, 2044, 4094] {
            let word = beq(1, 2, offset);
            let rebuilt = ((word >> 31) & 1) << 12
                | ((word >> 7) & 1) << 11
                | ((word >> 25) & 0x3F) << 5
                | ((word >> 8) & 0xF) << 1;
            let signed = ((rebuilt << 19) as i32) >> 19;
            assert_eq!(signed, offset, "beq {offset} did not survive encoding");
        }
    }

    #[test]
    fn a_jump_offset_survives_the_scattered_immediate() {
        for offset in [-1048576i32, -4, 0, 4, 2048, 1048574] {
            let word = jal(1, offset);
            let rebuilt = ((word >> 31) & 1) << 20
                | ((word >> 12) & 0xFF) << 12
                | ((word >> 20) & 1) << 11
                | ((word >> 21) & 0x3FF) << 1;
            let signed = ((rebuilt << 11) as i32) >> 11;
            assert_eq!(signed, offset, "jal {offset} did not survive encoding");
        }
    }

    #[test]
    fn the_reserved_opcode_is_not_a_real_one() {
        assert_eq!(RESERVED_OPCODE & 0x7F, 0);
        assert_eq!(NOP, 0x0000_0013);
    }

    #[test]
    fn a_program_packs_little_endian() {
        assert_eq!(program(&[0x0403_0201]), vec![0x01, 0x02, 0x03, 0x04]);
    }
}
