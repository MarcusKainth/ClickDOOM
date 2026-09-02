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

use crate::decode::Instruction;
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

/// What an observer says about carrying on.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Step {
    Continue,
    Stop,
}

/// What a run reports as it goes.
///
/// Every method does nothing by default, and the empty observer below costs
/// nothing: the loop is monomorphised, so a run recording none of this
/// compiles to the same code as one that could not record it.
pub trait Observer {
    /// Before an instruction executes, with the program counter on it and the
    /// registers as its caller left them. This is where a trap reads the
    /// arguments of the call it is watching.
    ///
    /// The machine is mutable so an observer can switch its own recording on,
    /// which is state the machine holds. An observer must not change
    /// architectural state: the run is the thing being measured.
    fn before_step(&mut self, _cpu: &mut Cpu) -> Step {
        Step::Continue
    }

    /// After an instruction retires, naming the program counter it ran at.
    fn after_step(&mut self, _cpu: &mut Cpu, _retired_pc: u32, _insn: Instruction) -> Step {
        Step::Continue
    }

    fn checkpoint(&mut self, _checkpoint: Checkpoint) {}
}

/// An observer that records nothing.
impl Observer for () {}

/// An observer held by reference, so a caller can keep its own handle on one
/// it has passed to a combinator and read what it recorded afterwards.
impl<O: Observer + ?Sized> Observer for &mut O {
    fn before_step(&mut self, cpu: &mut Cpu) -> Step {
        (**self).before_step(cpu)
    }

    fn after_step(&mut self, cpu: &mut Cpu, retired_pc: u32, insn: Instruction) -> Step {
        (**self).after_step(cpu, retired_pc, insn)
    }

    fn checkpoint(&mut self, checkpoint: Checkpoint) {
        (**self).checkpoint(checkpoint);
    }
}

/// An observer a caller may not have. `None` records nothing and costs a
/// null check per step, which is what lets one run take an optional second
/// observer without a second copy of the loop.
impl<O: Observer> Observer for Option<O> {
    fn before_step(&mut self, cpu: &mut Cpu) -> Step {
        self.as_mut().map_or(Step::Continue, |o| o.before_step(cpu))
    }

    fn after_step(&mut self, cpu: &mut Cpu, retired_pc: u32, insn: Instruction) -> Step {
        self.as_mut()
            .map_or(Step::Continue, |o| o.after_step(cpu, retired_pc, insn))
    }

    fn checkpoint(&mut self, checkpoint: Checkpoint) {
        if let Some(o) = self.as_mut() {
            o.checkpoint(checkpoint);
        }
    }
}

/// Two observers, both watching.
pub struct Both<A, B>(pub A, pub B);

impl<A: Observer, B: Observer> Observer for Both<A, B> {
    fn before_step(&mut self, cpu: &mut Cpu) -> Step {
        match (self.0.before_step(cpu), self.1.before_step(cpu)) {
            (Step::Continue, Step::Continue) => Step::Continue,
            _ => Step::Stop,
        }
    }

    fn after_step(&mut self, cpu: &mut Cpu, retired_pc: u32, insn: Instruction) -> Step {
        match (
            self.0.after_step(cpu, retired_pc, insn),
            self.1.after_step(cpu, retired_pc, insn),
        ) {
            (Step::Continue, Step::Continue) => Step::Continue,
            _ => Step::Stop,
        }
    }

    fn checkpoint(&mut self, checkpoint: Checkpoint) {
        self.0.checkpoint(checkpoint);
        self.1.checkpoint(checkpoint);
    }
}

/// Collects the trace, for a caller small enough to hold it.
#[derive(Default)]
pub struct Collector {
    pub lines: Vec<Checkpoint>,
}

impl Observer for Collector {
    fn checkpoint(&mut self, checkpoint: Checkpoint) {
        self.lines.push(checkpoint);
    }
}

/// Drives `cpu` to a stop, taking a checkpoint at each cadence point.
pub fn run<O: Observer>(cpu: &mut Cpu, config: TraceConfig, budget: u64, obs: &mut O) -> Stop {
    debug_assert!(config.validate().is_ok(), "the cadence hides a memory hash");
    while cpu.icount() < budget {
        if obs.before_step(cpu) == Step::Stop {
            return Stop::Observer;
        }
        let retired_pc = cpu.pc();
        let insn = match cpu.step_reporting() {
            Ok((_, insn)) => insn,
            Err(halt) => return Stop::Halted(halt),
        };
        if config.is_checkpoint(cpu.icount()) {
            obs.checkpoint(checkpoint_of(cpu, config.is_ram_hash(cpu.icount())));
        }
        if obs.after_step(cpu, retired_pc, insn) == Step::Stop {
            return Stop::Observer;
        }
    }
    Stop::Budget
}

/// Runs and collects, for a caller small enough to hold the whole trace.
pub fn collect(cpu: &mut Cpu, config: TraceConfig, budget: u64) -> (Vec<Checkpoint>, Stop) {
    let mut collector = Collector::default();
    let stop = run(cpu, config, budget, &mut collector);
    (collector.lines, stop)
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
        cpu.load_image(&program(&words), RAM_BASE).unwrap();
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

    #[derive(Default)]
    struct Watcher {
        counts: Vec<u64>,
        pcs: Vec<u32>,
        stop_at: Option<u64>,
    }

    impl Observer for Watcher {
        fn after_step(&mut self, cpu: &mut Cpu, retired_pc: u32, _insn: Instruction) -> Step {
            self.counts.push(cpu.icount());
            self.pcs.push(retired_pc);
            if self.stop_at == Some(cpu.icount()) {
                return Step::Stop;
            }
            Step::Continue
        }
    }

    #[test]
    fn the_observer_sees_every_retired_instruction_in_order() {
        let mut cpu = cpu_running(10, true);
        let mut watcher = Watcher::default();
        run(&mut cpu, SMALL, 100, &mut watcher);
        assert_eq!(watcher.counts, (1..=10).collect::<Vec<u64>>());
        assert_eq!(watcher.pcs[0], RAM_BASE);
        assert_eq!(watcher.pcs[9], RAM_BASE + 36);
    }

    #[test]
    fn the_observer_can_stop_the_run_where_it_chooses() {
        let mut cpu = cpu_running(64, false);
        let mut watcher = Watcher {
            stop_at: Some(5),
            ..Watcher::default()
        };
        let stop = run(&mut cpu, SMALL, 100, &mut watcher);
        assert_eq!(stop, Stop::Observer);
        assert_eq!(cpu.icount(), 5);
    }

    #[test]
    fn an_absent_observer_records_nothing_and_a_present_one_still_records() {
        let mut cpu = cpu_running(10, true);
        run(&mut cpu, SMALL, 100, &mut None::<Watcher>);
        assert_eq!(
            cpu.icount(),
            10,
            "None runs the machine and records nothing"
        );

        let mut cpu = cpu_running(10, true);
        let mut watcher = Some(Watcher::default());
        run(&mut cpu, SMALL, 100, &mut watcher);
        assert_eq!(watcher.unwrap().counts, (1..=10).collect::<Vec<u64>>());
    }

    #[test]
    fn an_observer_passed_by_reference_is_still_readable_afterwards() {
        let mut cpu = cpu_running(10, true);
        let mut watcher = Watcher::default();
        let mut collector = Collector::default();
        run(
            &mut cpu,
            SMALL,
            100,
            &mut Both(&mut watcher, &mut collector),
        );
        assert_eq!(watcher.counts.len(), 10);
        assert_eq!(collector.lines.len(), 1);
    }

    #[test]
    fn an_observer_that_records_nothing_still_runs_the_machine() {
        let mut cpu = cpu_running(10, true);
        assert!(run(&mut cpu, SMALL, 100, &mut ()).halt().is_some());
        assert_eq!(cpu.icount(), 10);
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
