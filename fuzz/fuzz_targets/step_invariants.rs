//! The machine holds its own invariants whatever it is fed.
//!
//! No oracle: each of these is a property the interpreter states about
//! itself, and a violation is a defect whatever another engine would say.
#![no_main]

use arbitrary::Arbitrary;
use clickdoom_spec::{IPMS_DEFAULT, RAM_BASE};
use libfuzzer_sys::fuzz_target;
use refemu::Cpu;
use refemu::asm::program;

const RAM_SIZE: u32 = 64 * 1024;

#[derive(Arbitrary, Debug)]
struct Case {
    words: Vec<u32>,
    regs: [u32; 32],
    pc_word: u8,
    steps: u8,
    declare_text: bool,
}

fuzz_target!(|case: Case| {
    if case.words.is_empty() || case.words.len() > 512 {
        return;
    }
    let map = clickdoom_spec::MemoryMap::clickdoom().with_ram_size(RAM_SIZE);
    let memory = refemu::Memory::new(map, refemu::Devices::registers(IPMS_DEFAULT));
    let mut cpu = Cpu::new(memory, RAM_BASE);
    cpu.load_image(&program(&case.words), RAM_BASE).unwrap();
    if case.declare_text {
        cpu.set_text_region(Some((RAM_BASE, RAM_BASE + 4 * case.words.len() as u32)));
        cpu.enable_decode_cache();
    }
    cpu.set_pc(RAM_BASE + 4 * u32::from(case.pc_word));
    for (index, value) in case.regs.iter().enumerate() {
        cpu.set_register(index as u8, *value);
    }

    for _ in 0..case.steps {
        let before = (cpu.icount(), cpu.pc(), *cpu.regs());
        match cpu.step() {
            Ok(()) => {
                assert_eq!(
                    cpu.icount(),
                    before.0 + 1,
                    "a retired instruction moved the count by something other than one"
                );
                assert_eq!(cpu.regs()[0], 0, "x0 was written");
                assert_eq!(
                    cpu.pc() % 4,
                    0,
                    "the program counter left a four-byte boundary"
                );
            }
            Err(halt) => {
                // A halt retires nothing: the count, the program counter and
                // every register are as they were.
                assert_eq!(cpu.icount(), before.0, "a halt retired the instruction");
                assert_eq!(cpu.pc(), before.1, "a halt moved the program counter");
                assert_eq!(*cpu.regs(), before.2, "a halt wrote a register");
                assert_eq!(halt.pc, before.1, "the halt names another instruction");
                // A fault carries no exit code, and a clean stop carries no
                // faulting address.
                if halt.reason.is_fault() {
                    assert!(halt.exit_code.is_none());
                } else {
                    assert!(halt.addr.is_none());
                }
                // Stepping again reports the same halt, since nothing moved.
                assert_eq!(cpu.step().unwrap_err(), halt);
                break;
            }
        }
    }
});
