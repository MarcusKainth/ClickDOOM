//! The reference RV32IM interpreter.
//!
//! This is the oracle. The SQL engine is checked against it instruction by
//! instruction, which only means something because the two share no execution
//! code. They share the contract, in `clickdoom-spec`, and nothing else.

pub mod asm;
pub mod boot;
pub mod cli;
pub mod decode;
pub mod exec;
pub mod image;
pub mod memory;
pub mod mmio;
pub mod predecode;
pub mod probe;
pub mod snapshot;
pub mod trace;

pub use boot::{BootReport, Outcome, boot, format_report};
pub use decode::{Instruction, Op, decode};
pub use exec::{Cpu, DidNotHalt, Halt};
pub use image::{Image, ImageError, Segment, Symbol, SymbolKind, read_image};
pub use memory::{LoadError, MemFault, Memory};
pub use mmio::{Devices, FrameCommit, KeyEvent, MmioExit, Registers};
pub use predecode::DecodeCache;
pub use trace::{
    Observer, Step, Stop, checkpoint_of, collect, fb_hash_of, ram_hash_of, reg_hash_of,
};
