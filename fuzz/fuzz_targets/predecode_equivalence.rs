//! The decode cache changes speed and nothing else.
//!
//! Coverage guidance is what makes this worth running: the cases that matter
//! are a jump landing inside the cached region and execution straddling its
//! end, and structured random generation mostly does not produce those.
#![no_main]

use arbitrary::Arbitrary;
use clickdoom_spec::{IPMS_DEFAULT, RAM_BASE, TraceConfig};
use libfuzzer_sys::fuzz_target;
use refemu::Cpu;
use refemu::asm::program;
use refemu::trace::collect;

const RAM_SIZE: u32 = 64 * 1024;

/// A cadence fine enough that a short run still produces lines to compare.
const TRACE: TraceConfig = TraceConfig {
    checkpoint_interval: 4,
    ram_hash_interval: 16,
};

#[derive(Arbitrary, Debug)]
struct Case {
    words: Vec<u32>,
    regs: [u32; 32],
    /// How much of the program the read-only region covers, in words.
    text_words: u8,
    pc_word: u8,
    steps: u8,
}

fn build(case: &Case, cache: bool) -> (Vec<clickdoom_spec::Checkpoint>, u64, u32, [u32; 32]) {
    let map = clickdoom_spec::MemoryMap::clickdoom().with_ram_size(RAM_SIZE);
    let memory = refemu::Memory::new(map, refemu::Devices::registers(IPMS_DEFAULT));
    let mut cpu = Cpu::new(memory, RAM_BASE);
    cpu.load_image(&program(&case.words), RAM_BASE).unwrap();
    let text_end = RAM_BASE + 4 * u32::from(case.text_words).min(case.words.len() as u32);
    if text_end > RAM_BASE {
        cpu.set_text_region(Some((RAM_BASE, text_end)));
    }
    cpu.set_pc(RAM_BASE + 4 * u32::from(case.pc_word));
    for (index, value) in case.regs.iter().enumerate() {
        cpu.set_register(index as u8, *value);
    }
    if cache {
        cpu.enable_decode_cache();
    }
    let (lines, _) = collect(&mut cpu, TRACE, u64::from(case.steps));
    (lines, cpu.icount(), cpu.pc(), *cpu.regs())
}

fuzz_target!(|case: Case| {
    if case.words.is_empty() || case.words.len() > 512 {
        return;
    }
    assert_eq!(
        build(&case, true),
        build(&case, false),
        "the cache changed the answer"
    );
});
