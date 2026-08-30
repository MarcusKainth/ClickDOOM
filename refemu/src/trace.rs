//! The checkpoint emitter.
//!
//! One loop drives every trace. A caller that needs to watch each retired
//! instruction, to catch a console milestone or a frame commit at the exact
//! count it lands on, passes an observer rather than writing a second loop
//! with its own copy of the cadence test. Two loops drift.
//!
//! A checkpoint is taken after the instruction retires, so it reports the
//! count and the program counter as they stand once the step is complete. A
//! halt emits no line: the trace records where the machine was, and a halt is
//! not somewhere it was.

use clickdoom_spec::{Checkpoint, TraceConfig, fb_hash, ram_hash, reg_hash};

use crate::exec::{Cpu, Halt};

/// The register hash of the machine as it stands.
pub fn reg_hash_of(cpu: &Cpu) -> u64 {
    reg_hash(cpu.pc(), cpu.regs())
}

/// The hash of the RAM region.
pub fn ram_hash_of(cpu: &Cpu) -> u64 {
    ram_hash(cpu.memory.ram())
}

/// The hash of the framebuffer followed by the palette.
pub fn fb_hash_of(cpu: &Cpu) -> u64 {
    fb_hash(cpu.memory.framebuffer(), cpu.memory.palette())
}

/// A checkpoint for the machine as it stands, with the memory hashes when
/// `with_memory` is set.
///
/// Both memory hashes appear together or not at all. They cover different
/// regions, and a divergence hunt that has one without the other cannot tell
/// which region moved.
pub fn checkpoint_of(cpu: &Cpu, with_memory: bool) -> Checkpoint {
    if with_memory {
        Checkpoint::with_memory(
            cpu.icount(),
            cpu.pc(),
            reg_hash_of(cpu),
            ram_hash_of(cpu),
            fb_hash_of(cpu),
        )
    } else {
        Checkpoint::registers_only(cpu.icount(), cpu.pc(), reg_hash_of(cpu))
    }
}

/// Why a traced run stopped.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Stop {
    /// The machine halted. This is the ordinary end of a real run.
    Halted(Halt),
    /// The instruction budget ran out with the machine still running.
    Budget,
    /// An observer asked to stop.
    Observer,
}

impl Stop {
    pub const fn halt(&self) -> Option<Halt> {
        match self {
            Stop::Halted(halt) => Some(*halt),
            _ => None,
        }
    }
}

/// What an observer says after each retired instruction.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Step {
    Continue,
    Stop,
}

/// Drives `cpu` to a stop, taking a checkpoint at each cadence point.
///
/// `on_step` sees every retired instruction. `on_checkpoint` sees the cadence.
/// A caller wanting only the trace passes an observer that does nothing, which
/// compiles away.
pub fn run<S, C>(
    cpu: &mut Cpu,
    config: TraceConfig,
    budget: u64,
    mut on_step: S,
    mut on_checkpoint: C,
) -> Stop
where
    S: FnMut(&Cpu) -> Step,
    C: FnMut(Checkpoint),
{
    debug_assert!(config.validate().is_ok(), "the cadence hides a memory hash");
    while cpu.icount() < budget {
        if let Err(halt) = cpu.step() {
            return Stop::Halted(halt);
        }
        if config.is_checkpoint(cpu.icount()) {
            on_checkpoint(checkpoint_of(cpu, config.is_ram_hash(cpu.icount())));
        }
        if on_step(cpu) == Step::Stop {
            return Stop::Observer;
        }
    }
    Stop::Budget
}

/// Runs and collects, for a caller small enough to hold the whole trace.
pub fn collect(cpu: &mut Cpu, config: TraceConfig, budget: u64) -> (Vec<Checkpoint>, Stop) {
    let mut lines = Vec::new();
    let stop = run(cpu, config, budget, |_| Step::Continue, |c| lines.push(c));
    (lines, stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::{addi, ecall, program};
    use clickdoom_spec::{HaltReason, RAM_BASE};

    /// A cadence small enough to reach in a test, keeping the ratio the real
    /// one has so the memory hashes still land on a checkpoint.
    const SMALL: TraceConfig = TraceConfig {
        checkpoint_interval: 8,
        ram_hash_interval: 32,
    };

    fn cpu_running(count: usize, then_halt: bool) -> Cpu {
        let mut cpu = Cpu::inert();
        let mut words = vec![addi(1, 1, 1); count];
        if then_halt {
            words.push(ecall());
        }
        cpu.memory.load_image(&program(&words), RAM_BASE).unwrap();
        cpu
    }

    #[test]
    fn a_checkpoint_lands_on_each_cadence_point_and_nowhere_else() {
        let mut cpu = cpu_running(8, true);
        let (lines, stop) = collect(&mut cpu, SMALL, 100);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].icount, 8);
        assert_eq!(stop.halt().unwrap().reason, HaltReason::Ecall);
    }

    #[test]
    fn the_memory_hashes_appear_only_at_their_own_cadence() {
        let mut cpu = cpu_running(24, true);
        let (lines, _) = collect(&mut cpu, SMALL, 100);
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert_eq!(line.field_count(), 3, "at icount {}", line.icount);
        }
    }

    #[test]
    fn a_memory_cadence_point_carries_both_hashes_from_real_machine_state() {
        let mut cpu = cpu_running(32, true);
        cpu.memory.write(RAM_BASE + 0x1000, 4, 0xABCD, 0).unwrap();
        let (lines, _) = collect(&mut cpu, SMALL, 100);
        let last = lines.last().unwrap();
        assert_eq!(last.icount, 32);
        assert_eq!(last.field_count(), 5);
        assert_eq!(last.ramhash, Some(ram_hash_of(&cpu)));
        assert_eq!(last.fbhash, Some(fb_hash_of(&cpu)));
    }

    #[test]
    fn the_framebuffer_hash_reads_the_machine_and_not_a_constant() {
        let mut cpu = cpu_running(32, true);
        cpu.memory
            .write(clickdoom_spec::FRAMEBUFFER_BASE, 4, 0xAB, 0)
            .unwrap();
        cpu.memory
            .write(clickdoom_spec::PALETTE_BASE, 4, 0xCD, 0)
            .unwrap();
        let (lines, _) = collect(&mut cpu, SMALL, 100);
        assert_eq!(lines.last().unwrap().fbhash, Some(fb_hash_of(&cpu)));
        assert_ne!(lines.last().unwrap().fbhash, lines.last().unwrap().ramhash);
    }

    #[test]
    fn running_out_of_budget_is_not_a_halt() {
        let mut cpu = cpu_running(64, false);
        let (lines, stop) = collect(&mut cpu, SMALL, 8);
        assert_eq!(lines.len(), 1);
        assert_eq!(stop, Stop::Budget);
        assert_eq!(stop.halt(), None);
    }

    #[test]
    fn a_halt_emits_no_line_for_the_partial_interval() {
        let mut cpu = cpu_running(3, true);
        let (lines, stop) = collect(&mut cpu, SMALL, 100);
        assert!(lines.is_empty());
        assert_eq!(stop.halt().unwrap().reason, HaltReason::Ecall);
        assert_eq!(cpu.icount(), 3);
    }

    #[test]
    fn the_observer_sees_every_retired_instruction_in_order() {
        let mut cpu = cpu_running(10, true);
        let mut seen = Vec::new();
        run(
            &mut cpu,
            SMALL,
            100,
            |cpu| {
                seen.push(cpu.icount());
                Step::Continue
            },
            |_| {},
        );
        assert_eq!(seen, (1..=10).collect::<Vec<u64>>());
    }

    #[test]
    fn the_observer_can_stop_the_run_where_it_chooses() {
        let mut cpu = cpu_running(64, false);
        let stop = run(
            &mut cpu,
            SMALL,
            100,
            |cpu| {
                if cpu.icount() == 5 {
                    Step::Stop
                } else {
                    Step::Continue
                }
            },
            |_| {},
        );
        assert_eq!(stop, Stop::Observer);
        assert_eq!(cpu.icount(), 5);
    }

    #[test]
    fn the_default_cadence_produces_the_committed_shape() {
        let config = TraceConfig::default();
        assert!(config.is_checkpoint(4_096));
        assert!(!config.is_ram_hash(4_096));
        assert!(config.is_ram_hash(1_048_576));
        assert_eq!(
            checkpoint_of(&Cpu::inert(), false).to_string(),
            format!("0\t{:08x}\t{:016x}", RAM_BASE, reg_hash(RAM_BASE, &[0; 32]))
        );
    }
}
