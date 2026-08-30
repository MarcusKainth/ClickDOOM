//! The closed halt-reason vocabulary.
//!
//! Every engine produces these spellings exactly: uppercase ASCII, no
//! punctuation, no per-engine normalisation. They are observable state that a
//! differential comparison matches on, so a synonym is a divergence.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
pub enum HaltReason {
    /// An opcode the machine does not implement.
    #[serde(rename = "ILLEGAL_INSN")]
    IllegalInsn,
    /// An access outside every declared region.
    #[serde(rename = "BAD_ADDR")]
    BadAddr,
    /// A store into the read-only text region.
    #[serde(rename = "SELF_MODIFY")]
    SelfModify,
    /// A half or word access not aligned to its own width, or a jump or branch
    /// computing a target that is not aligned to four bytes.
    #[serde(rename = "MISALIGNED")]
    Misaligned,
    #[serde(rename = "ECALL")]
    Ecall,
    #[serde(rename = "EBREAK")]
    Ebreak,
    /// Any of the six CSR instructions.
    #[serde(rename = "CSR")]
    Csr,
    /// The program's own clean stop, carrying an exit code. Not a fault.
    #[serde(rename = "EXIT")]
    Exit,
}

/// Every reason, in the order the vocabulary lists them.
pub const ALL_HALT_REASONS: [HaltReason; 8] = [
    HaltReason::IllegalInsn,
    HaltReason::BadAddr,
    HaltReason::SelfModify,
    HaltReason::Misaligned,
    HaltReason::Ecall,
    HaltReason::Ebreak,
    HaltReason::Csr,
    HaltReason::Exit,
];

impl HaltReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            HaltReason::IllegalInsn => "ILLEGAL_INSN",
            HaltReason::BadAddr => "BAD_ADDR",
            HaltReason::SelfModify => "SELF_MODIFY",
            HaltReason::Misaligned => "MISALIGNED",
            HaltReason::Ecall => "ECALL",
            HaltReason::Ebreak => "EBREAK",
            HaltReason::Csr => "CSR",
            HaltReason::Exit => "EXIT",
        }
    }

    /// A fault never carries an exit code, and `EXIT` never carries a fault
    /// reason, so the two are always separable.
    pub const fn is_fault(self) -> bool {
        !matches!(self, HaltReason::Exit)
    }
}

impl fmt::Display for HaltReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct UnknownHaltReason(pub String);

impl fmt::Display for UnknownHaltReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown halt reason {:?}", self.0)
    }
}

impl std::error::Error for UnknownHaltReason {}

impl FromStr for HaltReason {
    type Err = UnknownHaltReason;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ALL_HALT_REASONS
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| UnknownHaltReason(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_round_trips_through_its_spelling() {
        for reason in ALL_HALT_REASONS {
            assert_eq!(reason.as_str().parse::<HaltReason>(), Ok(reason));
        }
    }

    #[test]
    fn spellings_are_the_pinned_ones() {
        let spelled: Vec<&str> = ALL_HALT_REASONS.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            spelled,
            [
                "ILLEGAL_INSN",
                "BAD_ADDR",
                "SELF_MODIFY",
                "MISALIGNED",
                "ECALL",
                "EBREAK",
                "CSR",
                "EXIT",
            ]
        );
    }

    #[test]
    fn serde_uses_the_same_spellings() {
        for reason in ALL_HALT_REASONS {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, format!("\"{}\"", reason.as_str()));
            assert_eq!(
                serde_json::from_str::<HaltReason>(&json).unwrap(),
                reason,
                "{reason} did not survive a JSON round trip"
            );
        }
    }

    #[test]
    fn exit_is_the_only_reason_that_is_not_a_fault() {
        let not_faults: Vec<HaltReason> = ALL_HALT_REASONS
            .into_iter()
            .filter(|r| !r.is_fault())
            .collect();
        assert_eq!(not_faults, [HaltReason::Exit]);
    }

    #[test]
    fn an_unknown_spelling_is_an_error_rather_than_a_default() {
        assert!("exit".parse::<HaltReason>().is_err());
        assert!("ILLEGAL".parse::<HaltReason>().is_err());
        assert!("".parse::<HaltReason>().is_err());
    }
}
