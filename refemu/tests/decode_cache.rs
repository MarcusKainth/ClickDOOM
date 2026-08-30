//! The decode cache owes a proof: it changes speed and nothing else.
//!
//! Each case runs the same program twice, once with the region decoded up
//! front and once decoding on every fetch, and compares the whole checkpoint
//! trace and the final machine state. A cache that got an instruction wrong
//! would move a register, and a register moves the hash on the next line.

use clickdoom_spec::{Checkpoint, IPMS_DEFAULT, Manifest, RAM_BASE, TraceConfig};
use refemu::Cpu;
use refemu::asm::*;
use refemu::trace::{Stop, collect};

/// A cadence small enough that a short program still produces lines.
const FINE: TraceConfig = TraceConfig {
    checkpoint_interval: 4,
    ram_hash_interval: 16,
};

struct Run {
    lines: Vec<Checkpoint>,
    stop: Stop,
    icount: u64,
    pc: u32,
    regs: [u32; 32],
    cached: bool,
}

fn run(words: &[u32], text: Option<(u32, u32)>, budget: u64, cache: bool) -> Run {
    let mut cpu = Cpu::clickdoom(IPMS_DEFAULT);
    cpu.load_image(&program(words), RAM_BASE).unwrap();
    cpu.set_text_region(text);
    let cached = cache && cpu.enable_decode_cache();
    let (lines, stop) = collect(&mut cpu, FINE, budget);
    Run {
        lines,
        stop,
        icount: cpu.icount(),
        pc: cpu.pc(),
        regs: *cpu.regs(),
        cached,
    }
}

fn assert_same(name: &str, words: &[u32], text: Option<(u32, u32)>, budget: u64) {
    let with = run(words, text, budget, true);
    let without = run(words, text, budget, false);
    assert!(
        with.cached,
        "{name}: nothing was cached, so nothing was proved"
    );
    assert!(!without.cached);
    assert_eq!(with.lines, without.lines, "{name}: the traces differ");
    assert_eq!(
        with.stop, without.stop,
        "{name}: the runs ended differently"
    );
    assert_eq!(with.icount, without.icount, "{name}: the counts differ");
    assert_eq!(with.pc, without.pc, "{name}: the program counters differ");
    assert_eq!(with.regs, without.regs, "{name}: the registers differ");
    assert!(
        !with.lines.is_empty() || with.icount > 0,
        "{name}: the program retired nothing"
    );
}

#[test]
fn a_loop_inside_the_cached_region_runs_the_same_either_way() {
    // Counts down, storing and loading each time, so the memory path runs too.
    let words = vec![
        addi(1, 0, 40),
        lui(2, 0x80001),
        // loop:
        addi(1, 1, -1),
        sw(2, 1, 0),
        lw(3, 2, 0),
        add(4, 4, 3),
        bne(1, 0, -16),
        ecall(),
    ];
    let text = Some((RAM_BASE, RAM_BASE + (words.len() as u32) * 4));
    assert_same("loop", &words, text, 10_000);
}

#[test]
fn execution_leaving_and_re_entering_the_cached_region_runs_the_same_either_way() {
    // The region covers the first four words. The program jumps past it, runs
    // there, and comes back, so the run crosses the cache boundary twice.
    let outside = 8u32;
    let mut words = vec![
        addi(1, 0, 1),
        jal(5, (outside as i32) * 4 - 4),
        addi(2, 0, 2),
        ecall(),
    ];
    words.resize(outside as usize, nop());
    words.push(addi(3, 0, 3));
    words.push(jal(0, -(((outside as i32) + 1 - 2) * 4)));
    let text = Some((RAM_BASE, RAM_BASE + 16));
    assert_same("crossing", &words, text, 10_000);
}

#[test]
fn a_fault_reports_the_same_record_either_way() {
    for words in [
        vec![addi(1, 0, 1), RESERVED_OPCODE],
        vec![addi(1, 0, 1), ecall()],
        vec![lui(1, 0), lw(2, 1, 0)],
        vec![jal(1, 2)],
    ] {
        let text = Some((RAM_BASE, RAM_BASE + (words.len() as u32) * 4));
        let mut cached = Cpu::clickdoom(IPMS_DEFAULT);
        cached.load_image(&program(&words), RAM_BASE).unwrap();
        cached.set_text_region(text);
        assert!(cached.enable_decode_cache());

        let mut plain = Cpu::clickdoom(IPMS_DEFAULT);
        plain.load_image(&program(&words), RAM_BASE).unwrap();
        plain.set_text_region(text);

        let a = cached.run_until_halt(100).unwrap();
        let b = plain.run_until_halt(100).unwrap();
        assert_eq!(a, b, "the halt records differ for {words:?}");
        assert_eq!(cached.icount(), plain.icount());
    }
}

#[test]
fn a_machine_with_no_declared_region_caches_nothing_and_still_runs() {
    let words = [addi(1, 0, 1), ecall()];
    let mut cpu = Cpu::clickdoom(IPMS_DEFAULT);
    cpu.load_image(&program(&words), RAM_BASE).unwrap();
    assert!(!cpu.enable_decode_cache());
    assert!(cpu.decode_cache().is_none());
    assert!(cpu.run_until_halt(100).is_ok());
}

#[test]
fn loading_again_drops_what_was_decoded() {
    let words = [addi(1, 0, 1), ecall()];
    let mut cpu = Cpu::clickdoom(IPMS_DEFAULT);
    cpu.load_image(&program(&words), RAM_BASE).unwrap();
    cpu.set_text_region(Some((RAM_BASE, RAM_BASE + 8)));
    assert!(cpu.enable_decode_cache());
    cpu.load_image(&program(&[ecall(), ecall()]), RAM_BASE)
        .unwrap();
    assert!(
        cpu.decode_cache().is_none(),
        "a cache survived the bytes it described being replaced"
    );
    // Without the stale cache the machine reads what is actually there.
    assert_eq!(cpu.run_until_halt(10).unwrap().pc, RAM_BASE);
}

#[test]
fn declaring_a_different_region_drops_what_was_decoded() {
    let mut cpu = Cpu::clickdoom(IPMS_DEFAULT);
    cpu.load_image(&program(&[addi(1, 0, 1), ecall()]), RAM_BASE)
        .unwrap();
    cpu.set_text_region(Some((RAM_BASE, RAM_BASE + 8)));
    assert!(cpu.enable_decode_cache());
    cpu.set_text_region(None);
    assert!(cpu.decode_cache().is_none());
}

/// The real ROM, both ways, for as long as a test can afford.
///
/// This is the case that matters: real code, a real region, and a trace long
/// enough to cross a memory-hash boundary.
#[test]
fn the_real_rom_runs_the_same_either_way() {
    let image = std::path::Path::new("../rom/build/doom-rv32im.bin");
    if !image.exists() {
        // Not a skip. The suite that must not miss this is the one in CI,
        // which builds the ROM first; here it is a local convenience.
        eprintln!("# ../rom/build/doom-rv32im.bin is absent, so this case did not run");
        return;
    }
    let bytes = std::fs::read(image).unwrap();
    let manifest = Manifest::read(std::path::Path::new("../rom/build/manifest.json")).unwrap();
    let budget = 2_000_000;

    let mut traces = Vec::new();
    for cache in [true, false] {
        let mut cpu = Cpu::clickdoom(IPMS_DEFAULT);
        cpu.load_image(&bytes, manifest.load_addr.unwrap_or(RAM_BASE))
            .unwrap();
        cpu.set_text_region(manifest.text_region());
        let cached = cache && cpu.enable_decode_cache();
        assert_eq!(cached, cache, "the ROM's region should be cacheable");
        let config = TraceConfig {
            checkpoint_interval: 4_096,
            ram_hash_interval: 1_048_576,
        };
        traces.push((collect(&mut cpu, config, budget), cpu.icount(), *cpu.regs()));
    }
    assert_eq!(traces[0].0, traces[1].0, "the traces differ");
    assert_eq!(traces[0].1, traces[1].1);
    assert_eq!(traces[0].2, traces[1].2);
    let lines = traces[0].0.0.len();
    assert_eq!(lines, (budget / 4_096) as usize);
    assert!(
        traces[0].0.0.iter().any(|line| line.field_count() == 5),
        "the run never reached a memory-hash boundary, so memory was not compared"
    );
    println!("{lines} checkpoints agreed over {budget} instructions");
}
