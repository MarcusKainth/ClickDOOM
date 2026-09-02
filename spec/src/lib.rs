//! The contract every ClickDOOM engine has to agree on.
//!
//! This crate carries what `SPEC.md` and `NATIVE.md` state and nothing else:
//! the memory map, the MMIO register offsets, the halt-reason vocabulary, the
//! checkpoint format and its hashes, the ROM manifest and its pinned hash, and
//! the native-mode state row.
//!
//! It carries no decode and no execute logic. The reference emulator and the
//! SQL engine must stay independent implementations, because that is the only
//! reason a disagreement between them means anything. Sharing the constants
//! removes a transcription hazard; sharing the semantics would remove the
//! signal.

pub mod checkpoint;
pub mod halt;
pub mod manifest;
pub mod map;
pub mod native_state;

pub use checkpoint::{
    CHECKPOINT_INTERVAL, Checkpoint, RAM_HASH_INTERVAL, TraceConfig, XXH64_SEED, fb_hash, ram_hash,
    reg_hash,
};
pub use halt::HaltReason;
pub use manifest::{
    Manifest, PinnedHashError, Sha256Stream, assert_pinned_hash, hashed_filename, sha256_hex,
};
pub use map::{
    FRAMEBUFFER_BASE, FRAMEBUFFER_SIZE, IPMS_DEFAULT, MMIO_BASE, MMIO_SIZE, MemoryMap,
    PALETTE_BASE, PALETTE_SIZE, RAM_BASE, RAM_SIZE, Region, mmio,
};

/// The `spec_version` every artifact and table carries.
pub const SPEC_VERSION: &str = "0.1.0";
