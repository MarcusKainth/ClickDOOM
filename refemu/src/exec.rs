//! The machine: a register file, a program counter, and one instruction at a
//! time.
//!
//! This is the oracle, so every arm is a direct transcription of the base
//! ISA's instruction formats. Clarity beats cleverness here, because an arm
//! that is not obviously correct on reading is not done.
//!
//! A fatal halt does not retire the instruction that caused it. The count and
//! the program counter are unchanged, no register is written, and no memory
//! is modified. The halt record's pc identifies the faulting instruction. Both
//! engines key their checkpoints on that count, so being one out here shifts
//! every comparison downstream.

use clickdoom_spec::map::MemoryMap;
use clickdoom_spec::{HaltReason, RAM_BASE};

use crate::decode::{Instruction, Op, decode};
use crate::memory::{LoadError, MemFault, Memory};
use crate::mmio::Devices;
use crate::predecode::DecodeCache;

/// Why the machine stopped, and what the record carries.
///
/// `insn` is absent only when the fetch itself failed, since there is no
/// instruction word to report. `addr` names the faulting address, and
/// `exit_code` appears only on a clean stop.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Halt {
    pub reason: HaltReason,
    pub pc: u32,
    pub insn: Option<u32>,
    pub addr: Option<u32>,
    pub exit_code: Option<u32>,
}

impl Halt {
    const fn new(reason: HaltReason, pc: u32) -> Self {
        Self {
            reason,
            pc,
            insn: None,
            addr: None,
            exit_code: None,
        }
    }

    const fn with_insn(reason: HaltReason, pc: u32, insn: u32) -> Self {
        Self {
            insn: Some(insn),
            ..Self::new(reason, pc)
        }
    }

    const fn at(reason: HaltReason, pc: u32, insn: u32, addr: u32) -> Self {
        Self {
            insn: Some(insn),
            addr: Some(addr),
            ..Self::new(reason, pc)
        }
    }

    /// The instruction word is absent: the fetch is what failed.
    const fn on_fetch(reason: HaltReason, pc: u32, addr: u32) -> Self {
        Self {
            addr: Some(addr),
            ..Self::new(reason, pc)
        }
    }

    const fn exit(pc: u32, insn: u32, code: u32) -> Self {
        Self {
            insn: Some(insn),
            exit_code: Some(code),
            ..Self::new(HaltReason::Exit, pc)
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("did not halt within {0} instructions")]
pub struct DidNotHalt(pub u64);

pub struct Cpu {
    pub memory: Memory,
    pc: u32,
    regs: [u32; 32],
    icount: u64,
    /// Decoded instructions for the read-only region, when one is declared
    /// and the machine has been asked to keep them.
    cache: Option<DecodeCache>,
}

impl Cpu {
    /// Reset state: the program counter at the entry, every register zero.
    /// The program's own startup code sets the stack pointer, clears its
    /// zero-init section and reaches its entry point from there.
    pub fn new(memory: Memory, entry: u32) -> Self {
        Self {
            memory,
            pc: entry,
            regs: [0; 32],
            icount: 0,
            cache: None,
        }
    }

    pub const fn pc(&self) -> u32 {
        self.pc
    }

    pub const fn icount(&self) -> u64 {
        self.icount
    }

    pub const fn regs(&self) -> &[u32; 32] {
        &self.regs
    }

    /// Sets one register, for a debugger, a resume, or a test.
    pub const fn set_register(&mut self, index: u8, value: u32) {
        self.set_reg(index, value);
    }

    pub const fn set_pc(&mut self, pc: u32) {
        self.pc = pc;
    }

    /// Restores a machine captured mid-run.
    pub const fn restore(&mut self, pc: u32, regs: [u32; 32], icount: u64) {
        self.pc = pc;
        self.regs = regs;
        self.icount = icount;
        self.regs[0] = 0;
    }

    #[inline(always)]
    const fn reg(&self, index: u8) -> u32 {
        self.regs[index as usize]
    }

    /// Writes a register. `x0` reads as zero, so the write lands and then zero
    /// is put back, which costs a store rather than a branch.
    #[inline(always)]
    const fn set_reg(&mut self, index: u8, value: u32) {
        self.regs[index as usize] = value;
        self.regs[0] = 0;
    }

    /// A machine on the shipped map with the real device set.
    pub fn clickdoom(ipms: u32) -> Self {
        Self::new(
            Memory::new(MemoryMap::clickdoom(), Devices::registers(ipms)),
            RAM_BASE,
        )
    }

    /// A machine on the shipped map whose device window is plain bytes.
    /// The riscv-tests fixtures run this way: they have no device model to
    /// talk to, so a store landing there has to behave like memory.
    pub fn inert() -> Self {
        let map = MemoryMap::clickdoom();
        Self::new(Memory::new(map, Devices::bytes(map.mmio_size)), RAM_BASE)
    }

    /// Loads an image, dropping any decoded instructions it invalidates.
    pub fn load_image(&mut self, data: &[u8], load_addr: u32) -> Result<(), LoadError> {
        self.cache = None;
        self.memory.load_image(data, load_addr)
    }

    /// Declares the read-only region, dropping any decoded instructions.
    pub fn set_text_region(&mut self, region: Option<(u32, u32)>) {
        self.cache = None;
        self.memory.set_text_region(region);
    }

    /// Decodes the read-only region up front, so a fetch inside it is a
    /// lookup rather than a region dispatch and a decode.
    ///
    /// Call it after the image is loaded and the region declared. Doing either
    /// again drops the result. Returns whether anything was cached: a machine
    /// with no declared region has nothing to cache and runs the slow path.
    pub fn enable_decode_cache(&mut self) -> bool {
        self.cache = self
            .memory
            .text_region()
            .and_then(|(start, end)| DecodeCache::build(&self.memory, start, end));
        self.cache.is_some()
    }

    pub const fn decode_cache(&self) -> Option<&DecodeCache> {
        self.cache.as_ref()
    }

    /// Fetches, decodes and executes one instruction.
    #[inline]
    pub fn step(&mut self) -> Result<(), Halt> {
        self.step_reporting().map(|_| ())
    }

    /// Steps, reporting the instruction that retired.
    ///
    /// A caller that wants to name what ran, for a histogram or a trap, takes
    /// it from here rather than re-reading the word afterwards. A fetch is an
    /// ordinary read, so re-reading one that came from the device window
    /// would pop a second key event.
    #[inline]
    pub fn step_reporting(&mut self) -> Result<(u32, Instruction), Halt> {
        let pc = self.pc;
        let (word, insn) = match self.cache.as_ref().and_then(|cache| cache.get(pc)) {
            Some(entry) => (entry.word, entry.insn),
            None => {
                let word = match self.memory.read(pc, 4, self.icount) {
                    Ok(word) => word,
                    Err(fault) => return Err(fetch_halt(pc, fault)),
                };
                (word, decode(word))
            }
        };
        let next_pc = self.execute(pc, word, insn)?;
        self.icount += 1;
        self.pc = next_pc;
        Ok((word, insn))
    }

    /// Steps until the machine halts. The bound is a safety valve for a
    /// harness, not a machine concept: exceeding it means the program under
    /// test hangs, so it is an error rather than a quiet return.
    pub fn run_until_halt(&mut self, max_steps: u64) -> Result<Halt, DidNotHalt> {
        for _ in 0..max_steps {
            if let Err(halt) = self.step() {
                return Ok(halt);
            }
        }
        Err(DidNotHalt(max_steps))
    }

    fn execute(&mut self, pc: u32, word: u32, insn: Instruction) -> Result<u32, Halt> {
        let Instruction {
            op,
            rd,
            rs1,
            rs2,
            imm,
        } = insn;
        let next = pc.wrapping_add(4);

        match op {
            Op::Lui => self.set_reg(rd, imm as u32),
            Op::Auipc => self.set_reg(rd, pc.wrapping_add(imm as u32)),

            // A jump or branch computing a target that is not four-byte
            // aligned halts here, at the instruction that computed it, and
            // neither the program counter nor the link register is updated.
            // The jump does not architecturally complete.
            Op::Jal => {
                let target = pc.wrapping_add(imm as u32);
                check_target(target, pc, word)?;
                self.set_reg(rd, next);
                return Ok(target);
            }
            Op::Jalr => {
                let target = self.reg(rs1).wrapping_add(imm as u32) & !1;
                check_target(target, pc, word)?;
                self.set_reg(rd, next);
                return Ok(target);
            }
            Op::Beq | Op::Bne | Op::Blt | Op::Bge | Op::Bltu | Op::Bgeu => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                let taken = match op {
                    Op::Beq => a == b,
                    Op::Bne => a != b,
                    Op::Blt => (a as i32) < (b as i32),
                    Op::Bge => (a as i32) >= (b as i32),
                    Op::Bltu => a < b,
                    _ => a >= b,
                };
                if !taken {
                    // The fall-through is aligned already, because the
                    // program counter is aligned by invariant.
                    return Ok(next);
                }
                let target = pc.wrapping_add(imm as u32);
                check_target(target, pc, word)?;
                return Ok(target);
            }

            Op::Lb | Op::Lh | Op::Lw | Op::Lbu | Op::Lhu => {
                let addr = self.reg(rs1).wrapping_add(imm as u32);
                let (width, signed) = match op {
                    Op::Lb => (1, true),
                    Op::Lh => (2, true),
                    Op::Lw => (4, true),
                    Op::Lbu => (1, false),
                    _ => (2, false),
                };
                let raw = self
                    .memory
                    .read(addr, width, self.icount)
                    .map_err(|fault| access_halt(pc, word, fault))?;
                let value = if signed {
                    sign_extend(raw, width * 8)
                } else {
                    raw
                };
                self.set_reg(rd, value);
            }
            Op::Sb | Op::Sh | Op::Sw => {
                let addr = self.reg(rs1).wrapping_add(imm as u32);
                let width = match op {
                    Op::Sb => 1,
                    Op::Sh => 2,
                    _ => 4,
                };
                self.memory
                    .write(addr, width, self.reg(rs2), self.icount)
                    .map_err(|fault| access_halt(pc, word, fault))?;
            }

            Op::Addi => self.set_reg(rd, self.reg(rs1).wrapping_add(imm as u32)),
            Op::Slti => self.set_reg(rd, ((self.reg(rs1) as i32) < imm) as u32),
            Op::Sltiu => self.set_reg(rd, (self.reg(rs1) < imm as u32) as u32),
            Op::Xori => self.set_reg(rd, self.reg(rs1) ^ imm as u32),
            Op::Ori => self.set_reg(rd, self.reg(rs1) | imm as u32),
            Op::Andi => self.set_reg(rd, self.reg(rs1) & imm as u32),
            Op::Slli => self.set_reg(rd, self.reg(rs1) << (imm as u32 & 0x1F)),
            Op::Srli => self.set_reg(rd, self.reg(rs1) >> (imm as u32 & 0x1F)),
            Op::Srai => self.set_reg(rd, ((self.reg(rs1) as i32) >> (imm as u32 & 0x1F)) as u32),

            Op::Add => self.set_reg(rd, self.reg(rs1).wrapping_add(self.reg(rs2))),
            Op::Sub => self.set_reg(rd, self.reg(rs1).wrapping_sub(self.reg(rs2))),
            Op::Sll => self.set_reg(rd, self.reg(rs1) << (self.reg(rs2) & 0x1F)),
            Op::Slt => self.set_reg(rd, ((self.reg(rs1) as i32) < self.reg(rs2) as i32) as u32),
            Op::Sltu => self.set_reg(rd, (self.reg(rs1) < self.reg(rs2)) as u32),
            Op::Xor => self.set_reg(rd, self.reg(rs1) ^ self.reg(rs2)),
            Op::Srl => self.set_reg(rd, self.reg(rs1) >> (self.reg(rs2) & 0x1F)),
            Op::Sra => self.set_reg(
                rd,
                ((self.reg(rs1) as i32) >> (self.reg(rs2) & 0x1F)) as u32,
            ),
            Op::Or => self.set_reg(rd, self.reg(rs1) | self.reg(rs2)),
            Op::And => self.set_reg(rd, self.reg(rs1) & self.reg(rs2)),

            // The multiply extension. The edge cases are the whole job:
            // neither a division by zero nor the one overflowing division
            // traps, and the mixed-signedness high multiply takes its first
            // operand signed and its second unsigned.
            Op::Mul => self.set_reg(rd, self.reg(rs1).wrapping_mul(self.reg(rs2))),
            Op::Mulh => {
                let product = (self.reg(rs1) as i32 as i64) * (self.reg(rs2) as i32 as i64);
                self.set_reg(rd, (product >> 32) as u32);
            }
            Op::Mulhsu => {
                let product = (self.reg(rs1) as i32 as i64) * (self.reg(rs2) as i64);
                self.set_reg(rd, (product >> 32) as u32);
            }
            Op::Mulhu => {
                let product = (self.reg(rs1) as u64) * (self.reg(rs2) as u64);
                self.set_reg(rd, (product >> 32) as u32);
            }
            Op::Div => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                let result = if b == 0 {
                    u32::MAX
                } else if a == INT_MIN && b == MINUS_ONE {
                    INT_MIN
                } else {
                    ((a as i32) / (b as i32)) as u32
                };
                self.set_reg(rd, result);
            }
            Op::Divu => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                self.set_reg(rd, a.checked_div(b).unwrap_or(u32::MAX));
            }
            Op::Rem => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                let result = if b == 0 {
                    a
                } else if a == INT_MIN && b == MINUS_ONE {
                    0
                } else {
                    ((a as i32) % (b as i32)) as u32
                };
                self.set_reg(rd, result);
            }
            Op::Remu => {
                let (a, b) = (self.reg(rs1), self.reg(rs2));
                self.set_reg(rd, a.checked_rem(b).unwrap_or(a));
            }

            Op::Fence => {}

            Op::Ecall => return Err(Halt::with_insn(HaltReason::Ecall, pc, word)),
            Op::Ebreak => return Err(Halt::with_insn(HaltReason::Ebreak, pc, word)),
            Op::Csr => return Err(Halt::with_insn(HaltReason::Csr, pc, word)),
            Op::Illegal => return Err(Halt::with_insn(HaltReason::IllegalInsn, pc, word)),
        }
        Ok(next)
    }
}

/// The one dividend whose signed division overflows, and the divisor it
/// overflows against.
const INT_MIN: u32 = 0x8000_0000;
const MINUS_ONE: u32 = 0xFFFF_FFFF;

const fn sign_extend(value: u32, bits: u32) -> u32 {
    (((value << (32 - bits)) as i32) >> (32 - bits)) as u32
}

#[cold]
const fn check_target(target: u32, pc: u32, word: u32) -> Result<(), Halt> {
    if !target.is_multiple_of(4) {
        return Err(Halt::at(HaltReason::Misaligned, pc, word, target));
    }
    Ok(())
}

#[cold]
fn fetch_halt(pc: u32, fault: MemFault) -> Halt {
    match fault {
        MemFault::BadAddr { addr } => Halt::on_fetch(HaltReason::BadAddr, pc, addr),
        MemFault::Misaligned { addr, .. } => Halt::on_fetch(HaltReason::Misaligned, pc, addr),
        // A fetch is a read, so it cannot write into text, and the exit
        // register is write-only in the sense that reading it has no effect.
        MemFault::SelfModify { addr } => Halt::on_fetch(HaltReason::SelfModify, pc, addr),
        MemFault::Exit { code } => Halt {
            exit_code: Some(code),
            ..Halt::new(HaltReason::Exit, pc)
        },
    }
}

#[cold]
fn access_halt(pc: u32, word: u32, fault: MemFault) -> Halt {
    match fault {
        MemFault::BadAddr { addr } => Halt::at(HaltReason::BadAddr, pc, word, addr),
        MemFault::Misaligned { addr, .. } => Halt::at(HaltReason::Misaligned, pc, word, addr),
        MemFault::SelfModify { addr } => Halt::at(HaltReason::SelfModify, pc, word, addr),
        MemFault::Exit { code } => Halt::exit(pc, word, code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::*;
    use clickdoom_spec::map::mmio;
    use clickdoom_spec::{FRAMEBUFFER_BASE, MMIO_BASE, PALETTE_BASE, RAM_SIZE};

    /// Puts `words` in RAM at `base` and points the machine at them. Loading
    /// is not a store, so this works inside a declared text region.
    fn load_at(cpu: &mut Cpu, words: &[u32], base: u32) {
        cpu.load_image(&program(words), base).unwrap();
        cpu.set_pc(base);
    }

    fn load(cpu: &mut Cpu, words: &[u32]) {
        load_at(cpu, words, RAM_BASE);
    }

    fn run_one(words: &[u32]) -> Cpu {
        let mut cpu = Cpu::inert();
        load(&mut cpu, words);
        cpu.step().unwrap();
        cpu
    }

    fn halt_of(words: &[u32]) -> Halt {
        let mut cpu = Cpu::inert();
        load(&mut cpu, words);
        cpu.step().unwrap_err()
    }

    #[test]
    fn reset_state_is_the_entry_and_a_zeroed_register_file() {
        let cpu = Cpu::inert();
        assert_eq!(cpu.pc(), RAM_BASE);
        assert_eq!(cpu.regs(), &[0u32; 32]);
        assert_eq!(cpu.icount(), 0);
    }

    #[test]
    fn a_write_to_x0_is_discarded() {
        assert_eq!(run_one(&[addi(0, 0, 42)]).reg(0), 0);
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[addi(0, 0, -1); 5]);
        for _ in 0..5 {
            cpu.step().unwrap();
        }
        assert_eq!(cpu.reg(0), 0);
    }

    #[test]
    fn the_count_and_the_program_counter_advance_together() {
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[addi(1, 0, 1), addi(1, 1, 1)]);
        cpu.step().unwrap();
        assert_eq!((cpu.icount(), cpu.pc()), (1, RAM_BASE + 4));
        cpu.step().unwrap();
        assert_eq!((cpu.icount(), cpu.reg(1)), (2, 2));
    }

    #[test]
    fn lui_and_auipc() {
        assert_eq!(run_one(&[lui(1, 0xABCDE)]).reg(1), 0xABCD_E000);
        assert_eq!(run_one(&[auipc(1, 0x1)]).reg(1), RAM_BASE + 0x1000);
    }

    #[test]
    fn jal_links_and_jumps() {
        let cpu = run_one(&[jal(1, 0x100)]);
        assert_eq!(cpu.reg(1), RAM_BASE + 4);
        assert_eq!(cpu.pc(), RAM_BASE + 0x100);
    }

    #[test]
    fn jal_takes_a_negative_offset() {
        let mut cpu = Cpu::inert();
        load_at(&mut cpu, &[jal(0, -4)], RAM_BASE + 0x100);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x100 - 4);
    }

    #[test]
    fn jalr_clears_the_low_bit_of_its_target() {
        let mut cpu = Cpu::inert();
        cpu.set_register(2, RAM_BASE + 0x201);
        load(&mut cpu, &[jalr(1, 2, 0)]);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x200);
        assert_eq!(cpu.reg(1), RAM_BASE + 4);
    }

    #[test]
    fn jalr_reads_its_source_before_writing_the_link() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, RAM_BASE + 0x40);
        load(&mut cpu, &[jalr(1, 1, 4)]);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 0x44);
        assert_eq!(cpu.reg(1), RAM_BASE + 4);
    }

    #[test]
    fn every_branch_takes_and_falls_through_on_the_right_comparison() {
        type Enc = fn(u32, u32, i32) -> u32;
        let cases: &[(&str, Enc, u32, u32, bool)] = &[
            ("beq equal", beq, 5, 5, true),
            ("beq unequal", beq, 5, 6, false),
            ("bne unequal", bne, 5, 6, true),
            ("bne equal", bne, 5, 5, false),
            ("blt signed", blt, 0xFFFF_FFFF, 1, true),
            ("blt signed reversed", blt, 1, 0xFFFF_FFFF, false),
            ("bge signed", bge, 1, 0xFFFF_FFFF, true),
            ("bltu unsigned", bltu, 0xFFFF_FFFF, 1, false),
            ("bgeu unsigned", bgeu, 0xFFFF_FFFF, 1, true),
        ];
        for (name, enc, a, b, taken) in cases {
            let mut cpu = Cpu::inert();
            cpu.set_register(1, *a);
            cpu.set_register(2, *b);
            load(&mut cpu, &[enc(1, 2, 0x20)]);
            cpu.step().unwrap();
            let expected = RAM_BASE + if *taken { 0x20 } else { 4 };
            assert_eq!(cpu.pc(), expected, "{name}");
        }
    }

    #[test]
    fn loads_sign_extend_or_zero_extend_by_their_own_arm() {
        let mut cpu = Cpu::inert();
        cpu.memory.write(RAM_BASE + 0x40, 1, 0xFF, 0).unwrap();
        cpu.memory.write(RAM_BASE + 0x44, 2, 0xFFFF, 0).unwrap();
        cpu.set_register(1, RAM_BASE);
        for (word, expected) in [
            (lb(2, 1, 0x40), 0xFFFF_FFFF),
            (lbu(2, 1, 0x40), 0x0000_00FF),
            (lh(2, 1, 0x44), 0xFFFF_FFFF),
            (lhu(2, 1, 0x44), 0x0000_FFFF),
        ] {
            load(&mut cpu, &[word]);
            cpu.step().unwrap();
            assert_eq!(cpu.reg(2), expected);
        }
    }

    #[test]
    fn a_word_load_returns_the_bits_unchanged() {
        let mut cpu = Cpu::inert();
        cpu.memory
            .write(RAM_BASE + 0x40, 4, 0xDEAD_BEEF, 0)
            .unwrap();
        cpu.set_register(1, RAM_BASE);
        load(&mut cpu, &[lw(2, 1, 0x40)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(2), 0xDEAD_BEEF);
    }

    #[test]
    fn stores_write_only_their_own_width() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, RAM_BASE);
        cpu.set_register(2, 0xAABB_CCDD);
        for (word, addr, width, expected) in [
            (sb(1, 2, 0x40), RAM_BASE + 0x40, 1, 0xDD),
            (sh(1, 2, 0x44), RAM_BASE + 0x44, 2, 0xCCDD),
            (sw(1, 2, 0x48), RAM_BASE + 0x48, 4, 0xAABB_CCDD),
        ] {
            load(&mut cpu, &[word]);
            cpu.step().unwrap();
            assert_eq!(cpu.memory.read(addr, width, 0), Ok(expected));
        }
    }

    #[test]
    fn the_immediate_alu_arms() {
        let mut cpu = Cpu::inert();
        let cases: &[(u32, u32, u32)] = &[
            (10, addi(2, 1, -3), 7),
            (0xFFFF_FFFF, slti(2, 1, 0), 1),
            (0xFFFF_FFFF, sltiu(2, 1, 0), 0),
            (0b1010, xori(2, 1, 0b0110), 0b1100),
            (0b1010, ori(2, 1, 0b0110), 0b1110),
            (0b1010, andi(2, 1, 0b0110), 0b0010),
        ];
        for (rs1, word, expected) in cases {
            cpu.set_register(1, *rs1);
            load(&mut cpu, &[*word]);
            cpu.step().unwrap();
            assert_eq!(cpu.reg(2), *expected);
        }
    }

    #[test]
    fn a_right_shift_is_logical_or_arithmetic_by_its_own_arm() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, 1);
        load(&mut cpu, &[slli(2, 1, 4)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(2), 16);

        cpu.set_register(1, 0x8000_0000);
        load(&mut cpu, &[srli(2, 1, 4)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(2), 0x0800_0000);

        load(&mut cpu, &[srai(2, 1, 4)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(2), 0xF800_0000);
    }

    #[test]
    fn the_register_alu_arms() {
        let mut cpu = Cpu::inert();
        let cases: &[(u32, u32, u32, u32)] = &[
            (5, 3, add(3, 1, 2), 8),
            (5, 3, sub(3, 1, 2), 2),
            (0, 3, sub(3, 1, 2), 0xFFFF_FFFD),
            (0xFFFF_FFFF, 0, slt(3, 1, 2), 1),
            (0xFFFF_FFFF, 0, sltu(3, 1, 2), 0),
            (0b1010, 0b0110, xor(3, 1, 2), 0b1100),
            (0b1010, 0b0110, or(3, 1, 2), 0b1110),
            (0b1010, 0b0110, and(3, 1, 2), 0b0010),
            (0x8000_0000, 4, srl(3, 1, 2), 0x0800_0000),
            (0x8000_0000, 4, sra(3, 1, 2), 0xF800_0000),
            (0x8000_0000, 4, sll(3, 1, 2), 0),
        ];
        for (a, b, word, expected) in cases {
            cpu.set_register(1, *a);
            cpu.set_register(2, *b);
            load(&mut cpu, &[*word]);
            cpu.step().unwrap();
            assert_eq!(cpu.reg(3), *expected, "word {word:#010x}");
        }
    }

    #[test]
    fn a_shift_amount_uses_only_its_low_five_bits() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, 1);
        cpu.set_register(2, 33);
        load(&mut cpu, &[sll(3, 1, 2)]);
        cpu.step().unwrap();
        assert_eq!(cpu.reg(3), 2);
    }

    #[test]
    fn fence_retires_and_changes_nothing_else() {
        let cpu = run_one(&[fence()]);
        assert_eq!(cpu.pc(), RAM_BASE + 4);
        assert_eq!(cpu.icount(), 1);
        assert_eq!(cpu.regs(), &[0u32; 32]);
    }

    #[test]
    fn running_stops_at_the_first_halt() {
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[addi(1, 0, 1), addi(1, 1, 1), ecall()]);
        let halt = cpu.run_until_halt(10).unwrap();
        assert_eq!(halt.reason, HaltReason::Ecall);
        assert_eq!(cpu.reg(1), 2);
        assert_eq!(cpu.icount(), 2);
    }

    #[test]
    fn a_program_that_never_halts_is_an_error_rather_than_a_quiet_return() {
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[jal(0, 0)]);
        assert_eq!(cpu.run_until_halt(50), Err(DidNotHalt(50)));
    }

    // -- halts ------------------------------------------------------------

    #[test]
    fn a_reserved_opcode_is_illegal() {
        let halt = halt_of(&[RESERVED_OPCODE]);
        assert_eq!(halt.reason, HaltReason::IllegalInsn);
        assert_eq!(halt.pc, RAM_BASE);
        assert_eq!(halt.insn, Some(RESERVED_OPCODE));
    }

    #[test]
    fn an_unassigned_alu_function_is_illegal() {
        let halt = halt_of(&[r_type(0x33, 3, 0b000, 1, 2, 0x02)]);
        assert_eq!(halt.reason, HaltReason::IllegalInsn);
    }

    #[test]
    fn an_address_outside_every_region_is_a_bad_address() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, 0);
        load(&mut cpu, &[lw(2, 1, 0)]);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::BadAddr);
        assert_eq!(halt.addr, Some(0));
        assert_eq!(halt.pc, RAM_BASE);
        assert!(halt.insn.is_some());
    }

    #[test]
    fn a_store_just_past_ram_is_a_bad_address() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, RAM_BASE + RAM_SIZE);
        load(&mut cpu, &[sw(1, 2, 0)]);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::BadAddr);
        assert_eq!(halt.addr, Some(RAM_BASE + RAM_SIZE));
    }

    #[test]
    fn a_fetch_from_no_region_reports_no_instruction_word() {
        let mut cpu = Cpu::inert();
        cpu.set_pc(0x0000_1000);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::BadAddr);
        assert_eq!(halt.pc, 0x0000_1000);
        assert_eq!(halt.insn, None);
        assert_eq!(halt.addr, Some(0x0000_1000));
    }

    #[test]
    fn the_non_ram_regions_are_reachable_rather_than_bad_addresses() {
        for base in [MMIO_BASE, FRAMEBUFFER_BASE, PALETTE_BASE] {
            let mut cpu = Cpu::inert();
            cpu.set_register(1, base);
            load(&mut cpu, &[lw(2, 1, 0)]);
            assert!(cpu.step().is_ok(), "{base:#010x} is not readable");
        }
    }

    #[test]
    fn a_sub_word_store_to_the_pixel_regions_is_a_bad_address() {
        for base in [FRAMEBUFFER_BASE, PALETTE_BASE] {
            for word in [sb(1, 2, 0), sh(1, 2, 0)] {
                let mut cpu = Cpu::inert();
                cpu.set_register(1, base);
                cpu.set_register(2, 0xFF);
                load(&mut cpu, &[word]);
                let halt = cpu.step().unwrap_err();
                assert_eq!(halt.reason, HaltReason::BadAddr, "{base:#010x}");
                assert_eq!(halt.addr, Some(base));
            }
            let mut cpu = Cpu::inert();
            cpu.set_register(1, base);
            cpu.set_register(2, 0x0403_0201);
            load(&mut cpu, &[sw(1, 2, 0)]);
            cpu.step().unwrap();
            assert_eq!(cpu.memory.read(base, 4, 0), Ok(0x0403_0201));
        }
    }

    #[test]
    fn a_misaligned_data_access_halts() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, RAM_BASE + 1);
        load(&mut cpu, &[lw(2, 1, 0)]);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::Misaligned);
        assert_eq!(halt.addr, Some(RAM_BASE + 1));

        let mut cpu = Cpu::inert();
        cpu.set_register(1, RAM_BASE + 1);
        load(&mut cpu, &[sh(1, 2, 0)]);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::Misaligned);
    }

    #[test]
    fn a_byte_access_is_never_misaligned() {
        for offset in 0..4 {
            let mut cpu = Cpu::inert();
            cpu.set_register(1, RAM_BASE + offset);
            load(&mut cpu, &[lb(2, 1, 0)]);
            assert!(cpu.step().is_ok(), "offset {offset}");
        }
    }

    #[test]
    fn a_misaligned_jump_target_halts_at_the_jump_itself() {
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[jal(1, 2)]);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::Misaligned);
        assert_eq!(halt.pc, RAM_BASE, "the halt names the jump, not the target");
        assert_eq!(halt.addr, Some(RAM_BASE + 2));
        // Nothing about the jump takes effect.
        assert_eq!(cpu.pc(), RAM_BASE);
        assert_eq!(cpu.icount(), 0);
        assert_eq!(cpu.reg(1), 0);
    }

    #[test]
    fn a_misaligned_indirect_jump_target_halts_at_the_jump_itself() {
        let mut cpu = Cpu::inert();
        cpu.set_register(2, RAM_BASE + 2);
        load(&mut cpu, &[jalr(1, 2, 0)]);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::Misaligned);
        assert_eq!(halt.pc, RAM_BASE);
        assert_eq!(halt.addr, Some(RAM_BASE + 2));
        assert_eq!(cpu.reg(1), 0);
    }

    #[test]
    fn a_branch_not_taken_never_looks_at_its_target() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, 5);
        cpu.set_register(2, 6);
        load(&mut cpu, &[beq(1, 2, 2)]);
        cpu.step().unwrap();
        assert_eq!(cpu.pc(), RAM_BASE + 4);
    }

    #[test]
    fn a_branch_taken_to_a_misaligned_target_halts() {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, 5);
        cpu.set_register(2, 5);
        load(&mut cpu, &[beq(1, 2, 2)]);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::Misaligned);
        assert_eq!(halt.pc, RAM_BASE);
        assert_eq!(halt.addr, Some(RAM_BASE + 2));
    }

    #[test]
    fn a_store_into_the_text_region_is_a_self_modify() {
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[sw(1, 2, 0)]);
        cpu.memory
            .set_text_region(Some((RAM_BASE, RAM_BASE + 0x100)));
        cpu.set_register(1, RAM_BASE + 0x40);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::SelfModify);
        assert_eq!(halt.addr, Some(RAM_BASE + 0x40));
    }

    #[test]
    fn no_declared_text_region_leaves_a_store_landing() {
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[sw(1, 2, 0)]);
        cpu.set_register(1, RAM_BASE + 0x40);
        assert!(cpu.step().is_ok());
    }

    #[test]
    fn the_system_instructions_each_halt_with_their_own_reason() {
        for (word, reason) in [
            (ecall(), HaltReason::Ecall),
            (ebreak(), HaltReason::Ebreak),
            (csrrw(1, 0, 0x340), HaltReason::Csr),
        ] {
            let halt = halt_of(&[word]);
            assert_eq!(halt.reason, reason);
            assert_eq!(halt.insn, Some(word));
            assert_eq!(halt.pc, RAM_BASE);
        }
    }

    #[test]
    fn a_halt_retires_nothing() {
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[addi(1, 0, 7), ecall()]);
        cpu.step().unwrap();
        let (pc, icount, regs) = (cpu.pc(), cpu.icount(), *cpu.regs());
        cpu.step().unwrap_err();
        assert_eq!(cpu.pc(), pc);
        assert_eq!(cpu.icount(), icount);
        assert_eq!(cpu.regs(), &regs);
        // A second attempt reports the same halt, since nothing moved.
        assert_eq!(cpu.step().unwrap_err().reason, HaltReason::Ecall);
    }

    // -- the multiply extension -------------------------------------------

    /// Runs one register-register instruction over `a` and `b`, returning what
    /// it wrote.
    fn arith(word_of: fn(u32, u32, u32) -> u32, a: u32, b: u32) -> u32 {
        let mut cpu = Cpu::inert();
        cpu.set_register(1, a);
        cpu.set_register(2, b);
        load(&mut cpu, &[word_of(3, 1, 2)]);
        cpu.step().unwrap();
        cpu.reg(3)
    }

    const NEG: fn(i32) -> u32 = |v| v as u32;

    #[test]
    fn multiply_keeps_the_low_word_and_wraps() {
        assert_eq!(arith(mul, 6, 7), 42);
        assert_eq!(arith(mul, 0x10000, 0x10000), 0);
        assert_eq!(arith(mul, NEG(-6), 7), NEG(-42));
    }

    #[test]
    fn the_high_word_of_a_signed_multiply() {
        assert_eq!(arith(mulh, NEG(-1), NEG(-1)), 0);
        assert_eq!(arith(mulh, INT_MIN, INT_MIN), 0x4000_0000);
    }

    #[test]
    fn the_mixed_multiply_takes_its_first_operand_signed_and_its_second_unsigned() {
        assert_eq!(arith(mulhsu, NEG(-1), 2), u32::MAX);
        assert_eq!(arith(mulhsu, NEG(-1), u32::MAX), u32::MAX);
        // Reversing the operands is a different answer, which is what makes
        // getting the order backwards a silent divergence rather than a crash.
        assert_ne!(arith(mulhsu, 2, NEG(-1)), arith(mulhsu, NEG(-1), 2));
    }

    #[test]
    fn the_high_word_of_an_unsigned_multiply() {
        assert_eq!(arith(mulhu, u32::MAX, u32::MAX), 0xFFFF_FFFE);
    }

    #[test]
    fn division_truncates_toward_zero_rather_than_flooring() {
        assert_eq!(arith(div, 7, 2), 3);
        assert_eq!(arith(div, NEG(-7), 2), NEG(-3));
        assert_eq!(arith(div, 7, NEG(-2)), NEG(-3));
        assert_eq!(arith(div, NEG(-7), NEG(-2)), 3);
    }

    #[test]
    fn dividing_by_zero_gives_all_ones_and_does_not_trap() {
        assert_eq!(arith(div, 5, 0), u32::MAX);
        assert_eq!(arith(div, NEG(-5), 0), u32::MAX);
        assert_eq!(arith(divu, 5, 0), u32::MAX);
    }

    #[test]
    fn the_one_overflowing_division_gives_the_dividend_and_does_not_trap() {
        assert_eq!(arith(div, INT_MIN, MINUS_ONE), INT_MIN);
        assert_eq!(arith(rem, INT_MIN, MINUS_ONE), 0);
    }

    #[test]
    fn unsigned_division_reads_both_operands_unsigned() {
        assert_eq!(arith(divu, u32::MAX, 2), 0x7FFF_FFFF);
        assert_eq!(arith(remu, u32::MAX, 2), 1);
    }

    #[test]
    fn a_remainder_takes_the_sign_of_its_dividend() {
        assert_eq!(arith(rem, 7, 2), 1);
        assert_eq!(arith(rem, NEG(-7), 2), NEG(-1));
        assert_eq!(arith(rem, 7, NEG(-2)), 1);
        assert_eq!(arith(rem, NEG(-7), NEG(-2)), NEG(-1));
    }

    #[test]
    fn a_remainder_by_zero_gives_the_dividend_and_does_not_trap() {
        assert_eq!(arith(rem, 42, 0), 42);
        assert_eq!(arith(rem, NEG(-42), 0), NEG(-42));
        assert_eq!(arith(remu, u32::MAX, 0), u32::MAX);
    }

    #[test]
    fn a_multiply_is_not_an_illegal_instruction() {
        let mut cpu = Cpu::inert();
        load(&mut cpu, &[mul(3, 1, 2)]);
        assert!(cpu.step().is_ok());
    }

    #[test]
    fn writing_the_exit_register_stops_the_machine_with_its_code() {
        let mut cpu = Cpu::clickdoom(clickdoom_spec::IPMS_DEFAULT);
        cpu.set_register(1, MMIO_BASE + mmio::EXIT);
        cpu.set_register(2, 0xFFFF_FFFF);
        load(&mut cpu, &[sw(1, 2, 0)]);
        let halt = cpu.step().unwrap_err();
        assert_eq!(halt.reason, HaltReason::Exit);
        assert_eq!(halt.exit_code, Some(0xFFFF_FFFF));
        assert_eq!(halt.addr, None, "a clean stop names no faulting address");
        assert_eq!(cpu.icount(), 0, "the exit store does not retire");
    }
}
