//! What happened when a program ran: the outcome, the last few program
//! counters, and which instructions it spent its time on.
//!
//! The instruction mix answers a question the SQL engine asks: which decode
//! arms are worth collapsing. A mix taken from a synthetic profile answers it
//! wrongly, so this counts a real run.

use std::cmp::Reverse;
use std::fmt::Write as _;

use crate::exec::{Cpu, Halt};

/// How many recent program counters a fault report carries.
pub const RETIRED_PC_HISTORY: usize = 20;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The program announced a complete frame.
    FrameCommit,
    /// The machine stopped, cleanly or on a fault.
    Halt,
    /// The instruction budget ran out with the machine still running.
    BudgetExhausted,
}

impl Outcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Outcome::FrameCommit => "frame_commit",
            Outcome::Halt => "halt",
            Outcome::BudgetExhausted => "budget_exhausted",
        }
    }
}

pub struct BootReport {
    pub outcome: Outcome,
    pub icount: u64,
    /// Mnemonic and count, most-used first. Ties keep the order the
    /// instructions were first seen in.
    pub histogram: Vec<(&'static str, u64)>,
    /// The last retired program counters, oldest first.
    pub retired_pcs: Vec<u32>,
    pub halt: Option<Halt>,
    pub frame_no: Option<u32>,
}

/// Runs to the first announced frame, a halt, or the end of the budget.
pub fn boot(cpu: &mut Cpu, max_instructions: u64, pc_history: usize) -> BootReport {
    // First-seen order, so a tie in the sort below lands the way the run
    // encountered it.
    let mut order: Vec<&'static str> = Vec::new();
    let mut counts: Vec<u64> = Vec::new();
    let mut recent: std::collections::VecDeque<u32> = std::collections::VecDeque::new();

    while cpu.icount() < max_instructions {
        let pc_before = cpu.pc();
        let insn = match cpu.step_reporting() {
            Ok((_, insn)) => insn,
            Err(halt) => {
                return report(Outcome::Halt, cpu, order, counts, recent, Some(halt), None);
            }
        };

        if recent.len() == pc_history {
            recent.pop_front();
        }
        recent.push_back(pc_before);

        let name = insn.op.mnemonic();
        match order.iter().position(|seen| *seen == name) {
            Some(at) => counts[at] += 1,
            None => {
                order.push(name);
                counts.push(1);
            }
        }

        if let Some(commits) = cpu
            .memory
            .devices()
            .registers_ref()
            .map(|r| &r.frame_commits)
            && let Some(last) = commits.last()
        {
            let frame_no = last.frame_no;
            return report(
                Outcome::FrameCommit,
                cpu,
                order,
                counts,
                recent,
                None,
                Some(frame_no),
            );
        }
    }
    report(
        Outcome::BudgetExhausted,
        cpu,
        order,
        counts,
        recent,
        None,
        None,
    )
}

fn report(
    outcome: Outcome,
    cpu: &Cpu,
    order: Vec<&'static str>,
    counts: Vec<u64>,
    recent: std::collections::VecDeque<u32>,
    halt: Option<Halt>,
    frame_no: Option<u32>,
) -> BootReport {
    let mut histogram: Vec<(&'static str, u64)> = order.into_iter().zip(counts).collect();
    // A stable sort, so instructions with the same count stay in the order the
    // run first saw them.
    histogram.sort_by_key(|(_, count)| Reverse(*count));
    BootReport {
        outcome,
        icount: cpu.icount(),
        histogram,
        retired_pcs: recent.into_iter().collect(),
        halt,
        frame_no,
    }
}

impl BootReport {
    pub fn total_retired(&self) -> u64 {
        self.histogram.iter().map(|(_, count)| count).sum()
    }
}

pub fn format_report(report: &BootReport) -> String {
    let mut out = String::new();
    match report.outcome {
        Outcome::FrameCommit => {
            let _ = writeln!(
                out,
                "CLEAN RUN: reached FRAME_COMMIT (frame {}) at icount={}",
                report.frame_no.unwrap_or(0),
                report.icount
            );
        }
        Outcome::Halt => {
            let halt = report.halt.expect("a halt outcome carries a halt record");
            let _ = writeln!(
                out,
                "FAULT: {} at pc=0x{:08x} icount={}",
                halt.reason, halt.pc, report.icount
            );
            if let Some(insn) = halt.insn {
                let _ = writeln!(out, "  instruction word: 0x{insn:08x}");
            }
            if let Some(addr) = halt.addr {
                let _ = writeln!(out, "  address: 0x{addr:08x}");
            }
            if let Some(code) = halt.exit_code {
                let _ = writeln!(out, "  exit code: {code}");
            }
        }
        Outcome::BudgetExhausted => {
            let _ = writeln!(
                out,
                "BUDGET EXHAUSTED: no fault, no FRAME_COMMIT after icount={}",
                report.icount
            );
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "last {} retired pcs (oldest first):",
        report.retired_pcs.len()
    );
    for pc in &report.retired_pcs {
        let _ = writeln!(out, "  0x{pc:08x}");
    }

    let total = report.total_retired();
    let _ = writeln!(out);
    let _ = writeln!(out, "instruction mix ({total} retired):");
    for (mnemonic, count) in &report.histogram {
        let pct = if total == 0 {
            0.0
        } else {
            100.0 * *count as f64 / total as f64
        };
        let _ = writeln!(out, "  {mnemonic:<8} {pct:>6.2}%  ({count})");
    }
    // The Python renders this with a join, so there is no trailing newline.
    out.pop();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::{addi, ecall, program, sw};
    use clickdoom_spec::map::mmio;
    use clickdoom_spec::{HaltReason, IPMS_DEFAULT, MMIO_BASE, RAM_BASE};

    fn with(words: &[u32], real_devices: bool) -> Cpu {
        let mut cpu = if real_devices {
            Cpu::clickdoom(IPMS_DEFAULT)
        } else {
            Cpu::inert()
        };
        cpu.load_image(&program(words), RAM_BASE).unwrap();
        cpu
    }

    #[test]
    fn a_halt_is_reported_with_its_record_and_the_run_up_to_it() {
        let mut cpu = with(&[addi(1, 0, 1), addi(1, 1, 1), ecall()], false);
        let report = boot(&mut cpu, 100, RETIRED_PC_HISTORY);
        assert_eq!(report.outcome, Outcome::Halt);
        assert_eq!(report.icount, 2);
        assert_eq!(report.halt.unwrap().reason, HaltReason::Ecall);
        assert_eq!(report.histogram, vec![("addi", 2)]);
        assert_eq!(report.retired_pcs, vec![RAM_BASE, RAM_BASE + 4]);
    }

    #[test]
    fn an_announced_frame_ends_the_run() {
        let mut cpu = with(&[sw(1, 2, 0), addi(0, 0, 0)], true);
        cpu.set_register(1, MMIO_BASE + mmio::FRAME_COMMIT);
        cpu.set_register(2, 7);
        let report = boot(&mut cpu, 100, RETIRED_PC_HISTORY);
        assert_eq!(report.outcome, Outcome::FrameCommit);
        assert_eq!(report.frame_no, Some(7));
        assert_eq!(report.icount, 1, "the run ends on the announcing store");
    }

    #[test]
    fn running_out_of_budget_is_its_own_outcome() {
        let mut cpu = with(&[addi(1, 1, 1); 8], false);
        let report = boot(&mut cpu, 4, RETIRED_PC_HISTORY);
        assert_eq!(report.outcome, Outcome::BudgetExhausted);
        assert_eq!(report.icount, 4);
        assert!(report.halt.is_none());
    }

    #[test]
    fn the_program_counter_window_keeps_only_the_most_recent() {
        let mut cpu = with(&[addi(1, 1, 1); 32], false);
        let report = boot(&mut cpu, 30, 4);
        assert_eq!(report.retired_pcs.len(), 4);
        assert_eq!(report.retired_pcs[3], RAM_BASE + 29 * 4);
    }

    #[test]
    fn the_mix_is_most_used_first_and_ties_keep_the_order_they_were_seen() {
        let mut cpu = with(
            &[
                addi(1, 1, 1),
                crate::asm::or(2, 0, 0),
                addi(1, 1, 1),
                ecall(),
            ],
            false,
        );
        let report = boot(&mut cpu, 100, RETIRED_PC_HISTORY);
        assert_eq!(report.histogram, vec![("addi", 2), ("or", 1)]);
        assert_eq!(report.total_retired(), 3);
    }

    #[test]
    fn a_fault_report_names_only_the_fields_its_reason_carries() {
        let mut cpu = with(&[ecall()], false);
        let text = format_report(&boot(&mut cpu, 100, RETIRED_PC_HISTORY));
        assert!(text.starts_with("FAULT: ECALL at pc=0x80000000 icount=0\n"));
        assert!(text.contains("  instruction word: 0x00000073\n"));
        assert!(!text.contains("address:"), "{text}");
        assert!(!text.contains("exit code:"), "{text}");
    }

    #[test]
    fn a_clean_run_report_names_the_frame() {
        let mut cpu = with(&[sw(1, 2, 0)], true);
        cpu.set_register(1, MMIO_BASE + mmio::FRAME_COMMIT);
        cpu.set_register(2, 0);
        let text = format_report(&boot(&mut cpu, 100, RETIRED_PC_HISTORY));
        assert!(text.starts_with("CLEAN RUN: reached FRAME_COMMIT (frame 0) at icount=1\n"));
        assert!(text.contains("last 1 retired pcs (oldest first):\n  0x80000000\n"));
    }

    #[test]
    fn a_budget_report_says_neither_fault_nor_frame() {
        let mut cpu = with(&[addi(1, 1, 1); 4], false);
        let text = format_report(&boot(&mut cpu, 2, RETIRED_PC_HISTORY));
        assert!(text.starts_with("BUDGET EXHAUSTED: no fault, no FRAME_COMMIT after icount=2\n"));
        assert!(text.contains("instruction mix (2 retired):\n  addi     100.00%  (2)"));
    }
}
