//! The checkpoint trace: what an engine emits, how often, and over which bytes.
//!
//! Both engines must emit byte-identical trace files, so everything the format
//! leaves open is decided here.
//!
//! The seed is 0, which is what ClickHouse's `xxHash64` uses when called
//! without one. Hex fields are lowercase and zero-padded with no `0x` prefix:
//! eight digits for a pc, sixteen for a hash. ClickHouse's `hex()` is
//! uppercase and unpadded, so the SQL side pads and lowers to match, and this
//! is the easiest place for the two engines to diverge without noticing.
//!
//! The register hash covers `pc` followed by `x1` through `x31`, each a
//! four-byte little-endian word in register order. `x0` is never hashed: it is
//! always zero by construction, so it would add constant bytes and no signal.
//!
//! The RAM hash covers the RAM region alone, address-ascending. The
//! framebuffer hash covers the framebuffer followed by the palette, also
//! address-ascending. MMIO is in neither: it is live device state, not a value
//! two independently running engines should agree on bit for bit.

#[cfg(target_endian = "big")]
compile_error!("the checkpoint hashes read multi-byte values as little-endian");

use std::fmt;

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh64::{Xxh64, xxh64};

/// Retired instructions between checkpoints.
pub const CHECKPOINT_INTERVAL: u64 = 4_096;

/// Retired instructions between the memory hashes. A multiple of
/// `CHECKPOINT_INTERVAL`, so every memory hash lands on a checkpoint.
pub const RAM_HASH_INTERVAL: u64 = 1_048_576;

/// The seed ClickHouse's `xxHash64` uses when called without one.
pub const XXH64_SEED: u64 = 0;

/// The bytes `reg_hash` covers: a pc plus `x1` through `x31`.
pub const REG_HASH_BYTES: usize = 4 + 31 * 4;

/// How often a run emits. These are settings rather than constants so a test
/// can shrink them and put the emitter inside a comparison.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TraceConfig {
    pub checkpoint_interval: u64,
    pub ram_hash_interval: u64,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            checkpoint_interval: CHECKPOINT_INTERVAL,
            ram_hash_interval: RAM_HASH_INTERVAL,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TraceConfigError {
    #[error("checkpoint_interval is 0")]
    ZeroCheckpointInterval,
    #[error("ram_hash_interval is 0")]
    ZeroRamHashInterval,
    #[error(
        "ram_hash_interval {ram_hash_interval} is not a multiple of checkpoint_interval \
         {checkpoint_interval}, so a memory hash would land off a checkpoint"
    )]
    NotAMultiple {
        checkpoint_interval: u64,
        ram_hash_interval: u64,
    },
}

impl TraceConfig {
    pub fn validate(&self) -> Result<(), TraceConfigError> {
        if self.checkpoint_interval == 0 {
            return Err(TraceConfigError::ZeroCheckpointInterval);
        }
        if self.ram_hash_interval == 0 {
            return Err(TraceConfigError::ZeroRamHashInterval);
        }
        if !self
            .ram_hash_interval
            .is_multiple_of(self.checkpoint_interval)
        {
            return Err(TraceConfigError::NotAMultiple {
                checkpoint_interval: self.checkpoint_interval,
                ram_hash_interval: self.ram_hash_interval,
            });
        }
        Ok(())
    }

    /// Whether a checkpoint falls at this retired-instruction count. Zero is
    /// never a checkpoint: the test runs after a step, so the count is at
    /// least one by then.
    pub const fn is_checkpoint(&self, icount: u64) -> bool {
        icount != 0 && icount.is_multiple_of(self.checkpoint_interval)
    }

    /// Whether the memory hashes are appended at this count.
    pub const fn is_ram_hash(&self, icount: u64) -> bool {
        icount != 0 && icount.is_multiple_of(self.ram_hash_interval)
    }

    /// The next count at or after `icount` that emits, which is how far a run
    /// may go before it has to look at the trace again.
    pub const fn next_checkpoint_after(&self, icount: u64) -> u64 {
        icount + self.checkpoint_interval - (icount % self.checkpoint_interval)
    }
}

/// xxh64 over `pc` followed by `x1` through `x31`, each little-endian.
pub fn reg_hash(pc: u32, regs: &[u32; 32]) -> u64 {
    let mut buf = [0u8; REG_HASH_BYTES];
    buf[..4].copy_from_slice(&pc.to_le_bytes());
    for (i, reg) in regs[1..32].iter().enumerate() {
        let at = 4 + i * 4;
        buf[at..at + 4].copy_from_slice(&reg.to_le_bytes());
    }
    xxh64(&buf, XXH64_SEED)
}

/// xxh64 over the RAM region, address-ascending.
pub fn ram_hash(ram: &[u8]) -> u64 {
    xxh64(ram, XXH64_SEED)
}

/// xxh64 over the framebuffer followed by the palette, both
/// address-ascending. Streamed rather than concatenated, which gives the same
/// digest without copying 64,768 bytes per checkpoint.
pub fn fb_hash(framebuffer: &[u8], palette: &[u8]) -> u64 {
    let mut hasher = Xxh64::new(XXH64_SEED);
    hasher.update(framebuffer);
    hasher.update(palette);
    hasher.digest()
}

/// One trace line. The memory hashes are present together or not at all.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub icount: u64,
    pub pc: u32,
    pub reghash: u64,
    pub ramhash: Option<u64>,
    pub fbhash: Option<u64>,
}

impl Checkpoint {
    pub const fn registers_only(icount: u64, pc: u32, reghash: u64) -> Self {
        Self {
            icount,
            pc,
            reghash,
            ramhash: None,
            fbhash: None,
        }
    }

    pub const fn with_memory(
        icount: u64,
        pc: u32,
        reghash: u64,
        ramhash: u64,
        fbhash: u64,
    ) -> Self {
        Self {
            icount,
            pc,
            reghash,
            ramhash: Some(ramhash),
            fbhash: Some(fbhash),
        }
    }

    /// The number of tab-separated fields this renders to.
    pub const fn field_count(&self) -> usize {
        if self.ramhash.is_some() { 5 } else { 3 }
    }
}

impl fmt::Display for Checkpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\t{:08x}\t{:016x}", self.icount, self.pc, self.reghash)?;
        if let Some(ramhash) = self.ramhash {
            write!(f, "\t{ramhash:016x}")?;
        }
        if let Some(fbhash) = self.fbhash {
            write!(f, "\t{fbhash:016x}")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseCheckpointError {
    #[error("a checkpoint has 3 or 5 tab-separated fields, this line has {0}")]
    FieldCount(usize),
    #[error("field {field} is {value:?}, which is not {expected}")]
    Field {
        field: &'static str,
        value: String,
        expected: &'static str,
    },
}

impl std::str::FromStr for Checkpoint {
    type Err = ParseCheckpointError;

    /// Parses a line the trace emitter wrote. Hex fields must carry their full
    /// width in lowercase, because that padding is the part of the format two
    /// engines are most likely to disagree on and a lenient parser would hide
    /// it.
    fn from_str(line: &str) -> Result<Self, Self::Err> {
        fn hex(
            field: &'static str,
            value: &str,
            digits: usize,
        ) -> Result<u64, ParseCheckpointError> {
            let bad = || ParseCheckpointError::Field {
                field,
                value: value.to_owned(),
                expected: if digits == 8 {
                    "8 lowercase hex digits"
                } else {
                    "16 lowercase hex digits"
                },
            };
            if value.len() != digits
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(bad());
            }
            u64::from_str_radix(value, 16).map_err(|_| bad())
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 3 && fields.len() != 5 {
            return Err(ParseCheckpointError::FieldCount(fields.len()));
        }
        let icount = fields[0]
            .parse::<u64>()
            .map_err(|_| ParseCheckpointError::Field {
                field: "icount",
                value: fields[0].to_owned(),
                expected: "a decimal count",
            })?;
        // A leading zero would render differently, so reject it rather than
        // accept two spellings of one count.
        if fields[0] != icount.to_string() {
            return Err(ParseCheckpointError::Field {
                field: "icount",
                value: fields[0].to_owned(),
                expected: "a decimal count without padding",
            });
        }
        let pc = hex("pc", fields[1], 8)? as u32;
        let reghash = hex("reghash", fields[2], 16)?;
        let (ramhash, fbhash) = if fields.len() == 5 {
            (
                Some(hex("ramhash", fields[3], 16)?),
                Some(hex("fbhash", fields[4], 16)?),
            )
        } else {
            (None, None)
        };
        Ok(Self {
            icount,
            pc,
            reghash,
            ramhash,
            fbhash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_the_two_regions_matches_hashing_their_concatenation() {
        let fb: Vec<u8> = (0..64_000u32).map(|i| (i % 251) as u8).collect();
        let palette: Vec<u8> = (0..768u32).map(|i| (i % 253) as u8).collect();
        let mut joined = fb.clone();
        joined.extend_from_slice(&palette);
        assert_eq!(fb_hash(&fb, &palette), xxh64(&joined, XXH64_SEED));
    }

    #[test]
    fn reg_hash_covers_exactly_pc_and_x1_through_x31() {
        assert_eq!(REG_HASH_BYTES, 128);
        let mut regs = [0u32; 32];
        regs[0] = 0xFFFF_FFFF;
        // x0 is not hashed, so poking it changes nothing.
        assert_eq!(
            reg_hash(0x8000_0004, &regs),
            reg_hash(0x8000_0004, &[0; 32])
        );
    }

    #[test]
    fn reg_hash_reads_every_hashed_register() {
        let base = reg_hash(0x8000_0000, &[0; 32]);
        for i in 1..32 {
            let mut regs = [0u32; 32];
            regs[i] = 1;
            assert_ne!(reg_hash(0x8000_0000, &regs), base, "x{i} is not hashed");
        }
    }

    #[test]
    fn fb_hash_is_order_sensitive() {
        let fb: Vec<u8> = (0..16u8).collect();
        let palette: Vec<u8> = (200..208u8).collect();
        assert_ne!(fb_hash(&fb, &palette), fb_hash(&palette, &fb));
    }

    #[test]
    fn a_line_renders_with_lowercase_zero_padded_hex() {
        assert_eq!(
            Checkpoint::registers_only(4096, 0x8000_1000, 0x1234_5678_9ABC_DEF0).to_string(),
            "4096\t80001000\t123456789abcdef0"
        );
        assert_eq!(
            Checkpoint::with_memory(1_048_576, 0x8000_2000, 0xFF, 0xFF, 0xAB).to_string(),
            "1048576\t80002000\t00000000000000ff\t00000000000000ff\t00000000000000ab"
        );
        assert_eq!(
            Checkpoint::registers_only(1, 0, 0).to_string(),
            "1\t00000000\t0000000000000000"
        );
    }

    #[test]
    fn a_line_round_trips_through_the_parser() {
        for checkpoint in [
            Checkpoint::registers_only(4096, 0x8000_1000, 0x1234_5678_9ABC_DEF0),
            Checkpoint::with_memory(1_048_576, 0x8000_2000, 0xFF, 0xAB, 0xCD),
        ] {
            let line = checkpoint.to_string();
            assert_eq!(line.parse::<Checkpoint>().unwrap(), checkpoint);
        }
    }

    #[test]
    fn the_parser_rejects_the_shapes_the_emitter_never_writes() {
        for line in [
            "4096\t80001000",                       // too few fields
            "4096\t80001000\t123456789abcdef0\tff", // four fields
            "4096\t8001000\t123456789abcdef0",      // short pc
            "4096\t80001000\t123456789ABCDEF0",     // uppercase hash
            "4096\t80001000\t123456789abcdef",      // short hash
            "04096\t80001000\t123456789abcdef0",    // padded icount
            "4096 80001000 123456789abcdef0",       // spaces, not tabs
        ] {
            assert!(
                line.parse::<Checkpoint>().is_err(),
                "the parser accepted {line:?}"
            );
        }
    }

    #[test]
    fn the_default_cadence_is_the_pinned_one() {
        let config = TraceConfig::default();
        assert_eq!(config.checkpoint_interval, 4_096);
        assert_eq!(config.ram_hash_interval, 1_048_576);
        assert_eq!(config.ram_hash_interval % config.checkpoint_interval, 0);
        config.validate().unwrap();
    }

    #[test]
    fn icount_zero_is_not_a_checkpoint() {
        let config = TraceConfig::default();
        assert!(!config.is_checkpoint(0));
        assert!(!config.is_ram_hash(0));
        assert!(config.is_checkpoint(4_096));
        assert!(!config.is_checkpoint(4_095));
        assert!(config.is_ram_hash(1_048_576));
        assert!(config.is_checkpoint(1_048_576));
    }

    #[test]
    fn the_next_checkpoint_is_strictly_ahead() {
        let config = TraceConfig::default();
        assert_eq!(config.next_checkpoint_after(0), 4_096);
        assert_eq!(config.next_checkpoint_after(1), 4_096);
        assert_eq!(config.next_checkpoint_after(4_095), 4_096);
        assert_eq!(config.next_checkpoint_after(4_096), 8_192);
    }

    #[test]
    fn a_cadence_that_would_hide_a_memory_hash_is_rejected() {
        assert_eq!(
            TraceConfig {
                checkpoint_interval: 4_096,
                ram_hash_interval: 5_000,
            }
            .validate(),
            Err(TraceConfigError::NotAMultiple {
                checkpoint_interval: 4_096,
                ram_hash_interval: 5_000,
            })
        );
        assert_eq!(
            TraceConfig {
                checkpoint_interval: 0,
                ram_hash_interval: 1,
            }
            .validate(),
            Err(TraceConfigError::ZeroCheckpointInterval)
        );
    }
}
