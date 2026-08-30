//! What a run writes for another program to read.
//!
//! Every field is always present, `null` where it does not apply, so one
//! schema covers a run that halted and a run that did not and a reader never
//! has to tell which shape it got. Each hash and address comes with a hex
//! twin, so a shell does not have to reformat one to compare it.

use clickdoom_spec::HaltReason;
use serde::{Deserialize, Serialize};

use crate::exec::Halt;
use crate::mmio::FrameCommit;

fn hex8(value: Option<u32>) -> Option<String> {
    value.map(|v| format!("{v:08x}"))
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HaltJson {
    pub reason: HaltReason,
    pub pc: u32,
    pub pc_hex: String,
    pub icount: u64,
    pub insn: Option<u32>,
    pub insn_hex: Option<String>,
    pub addr: Option<u32>,
    pub addr_hex: Option<String>,
    pub exit_code: Option<u32>,
}

impl HaltJson {
    pub fn new(halt: Halt, icount: u64) -> Self {
        Self {
            reason: halt.reason,
            pc: halt.pc,
            pc_hex: format!("{:08x}", halt.pc),
            icount,
            insn: halt.insn,
            insn_hex: hex8(halt.insn),
            addr: halt.addr,
            addr_hex: hex8(halt.addr),
            exit_code: halt.exit_code,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FrameCommitJson {
    /// Position in the sequence of announced frames, counting from zero.
    pub index: u64,
    /// The number the program wrote, which is the program's to choose.
    pub frame_no: u32,
    /// The retired count before the announcing store completes.
    pub commit_icount: u64,
    /// The retired count after it, which is what a checkpoint reports.
    pub retired_icount: u64,
}

impl FrameCommitJson {
    pub const fn new(index: u64, commit: FrameCommit) -> Self {
        Self {
            index,
            frame_no: commit.frame_no,
            commit_icount: commit.commit_icount,
            retired_icount: commit.retired_icount(),
        }
    }
}

/// How a run ended.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// A stop condition the caller asked for.
    Stop,
    /// The machine halted.
    Halt,
    /// The instruction budget ran out.
    Budget,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RunReport {
    pub schema: String,
    pub outcome: RunOutcome,
    /// The condition that fired, in the grammar the flags take.
    pub stop_condition: Option<String>,
    pub halted: bool,
    pub icount: u64,
    pub pc: u32,
    pub pc_hex: String,
    pub halt: Option<HaltJson>,
    /// The hashes at the stop point, whether or not it lands on a checkpoint.
    /// A run that ends anywhere else leaves the trace's last hash-bearing line
    /// stale by up to one interval.
    pub reghash: String,
    pub ramhash: String,
    pub fbhash: String,
    pub frame_commit_count: u64,
    pub first_frame_commit: Option<FrameCommitJson>,
    pub last_frame_commit: Option<FrameCommitJson>,
    pub console_bytes: u64,
    pub rom_sha256: Option<String>,
    pub pinned: bool,
}

pub const RUN_REPORT_SCHEMA: &str = "refemu.run-report/1";

pub fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> std::io::Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    if path == std::path::Path::new("-") {
        use std::io::Write as _;
        std::io::stdout().write_all(text.as_bytes())
    } else {
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_halt_record_carries_a_hex_twin_for_every_address() {
        let halt = Halt {
            reason: HaltReason::BadAddr,
            pc: 0x8000_0004,
            insn: Some(0x0000_0073),
            addr: Some(0),
            exit_code: None,
        };
        let json = HaltJson::new(halt, 42);
        assert_eq!(json.pc_hex, "80000004");
        assert_eq!(json.insn_hex.as_deref(), Some("00000073"));
        assert_eq!(json.addr_hex.as_deref(), Some("00000000"));
        assert_eq!(json.exit_code, None);
    }

    #[test]
    fn a_missing_field_serialises_as_null_rather_than_disappearing() {
        let halt = Halt {
            reason: HaltReason::Ecall,
            pc: 0,
            insn: None,
            addr: None,
            exit_code: None,
        };
        let text = serde_json::to_string(&HaltJson::new(halt, 0)).unwrap();
        assert!(text.contains("\"addr\":null"), "{text}");
        assert!(text.contains("\"exit_code\":null"), "{text}");
        assert!(text.contains("\"reason\":\"ECALL\""), "{text}");
    }

    #[test]
    fn a_frame_commit_reports_both_counts_by_name() {
        let json = FrameCommitJson::new(
            0,
            FrameCommit {
                frame_no: 0,
                commit_icount: 15_393_135,
            },
        );
        assert_eq!(json.commit_icount, 15_393_135);
        assert_eq!(json.retired_icount, 15_393_136);
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let report = RunReport {
            schema: RUN_REPORT_SCHEMA.to_owned(),
            outcome: RunOutcome::Budget,
            stop_condition: None,
            halted: false,
            icount: 10,
            pc: 0x8000_0000,
            pc_hex: "80000000".to_owned(),
            halt: None,
            reghash: "0000000000000000".to_owned(),
            ramhash: "0000000000000001".to_owned(),
            fbhash: "0000000000000002".to_owned(),
            frame_commit_count: 0,
            first_frame_commit: None,
            last_frame_commit: None,
            console_bytes: 0,
            rom_sha256: None,
            pinned: false,
        };
        let text = serde_json::to_string(&report).unwrap();
        assert_eq!(serde_json::from_str::<RunReport>(&text).unwrap(), report);
    }
}
