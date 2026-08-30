//! The official rv32ui and rv32um suites.
//!
//! Each fixture is a flat binary built against a minimal environment, because
//! the upstream one starts by writing machine-mode control registers, which
//! this machine stops on. A fixture signals its result the way the suite
//! does: an environment call, with zero in `a0` for a pass and the failing
//! case number encoded in it otherwise.
//!
//! These are the only checks on this interpreter that come from outside the
//! project.

use std::path::{Path, PathBuf};

use clickdoom_spec::{HaltReason, RAM_BASE};
use refemu::{Cpu, Op};

/// The suite the fixture builder produces. Asserted rather than trusted,
/// because a path typo turns the loop below into a green run over nothing.
const EXPECTED_FIXTURES: usize = 48;

/// Every fixture halts well inside this. It is a hang detector, not a budget.
const MAX_STEPS: u64 = 200_000;

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("riscv_tests");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "bin"))
        .collect();
    found.sort();
    found
}

fn name_of(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

#[test]
fn the_fixture_set_is_the_size_the_builder_produces() {
    let found = fixtures();
    assert_eq!(
        found.len(),
        EXPECTED_FIXTURES,
        "expected {EXPECTED_FIXTURES} fixtures, found {}. Did the directory move, \
         or did scripts/build_riscv_tests.sh not run?",
        found.len()
    );
}

#[test]
fn every_riscv_test_passes() {
    let found = fixtures();
    assert_eq!(
        found.len(),
        EXPECTED_FIXTURES,
        "the fixture set changed size"
    );

    let mut failures: Vec<String> = Vec::new();
    let mut passed = 0usize;
    let mut retired_total = 0u64;

    for path in &found {
        let name = name_of(path);
        let image = std::fs::read(path).unwrap();
        let mut cpu = Cpu::inert();
        cpu.load_image(&image, RAM_BASE).unwrap();

        // Stepping one at a time rather than running to a halt, so every
        // retired instruction can be checked against the decoder as well.
        let mut halt = None;
        let mut retired = 0u64;
        for _ in 0..MAX_STEPS {
            match cpu.step_reporting() {
                Ok((word, insn)) => {
                    assert_ne!(
                        insn.op,
                        Op::Illegal,
                        "{name} retired {word:#010x} at icount {} which the decoder calls illegal",
                        cpu.icount()
                    );
                    retired += 1;
                }
                Err(h) => {
                    halt = Some(h);
                    break;
                }
            }
        }
        retired_total += retired;

        let Some(halt) = halt else {
            failures.push(format!(
                "{name}: did not halt within {MAX_STEPS} instructions"
            ));
            continue;
        };
        if halt.reason != HaltReason::Ecall {
            failures.push(format!(
                "{name}: expected a clean ECALL exit, got {} at pc={:#010x} (icount={})",
                halt.reason,
                halt.pc,
                cpu.icount()
            ));
            continue;
        }
        let a0 = cpu.regs()[10];
        if a0 != 0 {
            // The suite encodes the failing case number as (number << 1) | 1
            // before writing it to a0.
            failures.push(format!(
                "{name}: case {} failed (a0={a0:#x}, icount={})",
                (a0 - 1) >> 1,
                cpu.icount()
            ));
            continue;
        }
        println!("{name} ... ok ({retired} instructions)");
        passed += 1;
    }

    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    assert_eq!(passed, EXPECTED_FIXTURES);
    // A loop that retires almost nothing would report the same green run, so
    // say how much was actually executed.
    assert!(
        retired_total > 10_000,
        "the suite retired only {retired_total} instructions"
    );
    println!("{passed}/{EXPECTED_FIXTURES} fixtures, {retired_total} instructions retired");
}
