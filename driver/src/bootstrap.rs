//! Seeding `batch_commit`'s `batch_id = 0` row: the reset state (`pc =
//! RAM_BASE`, `x1..x31 = 0`), so the first real batch's lookup of the
//! previous batch has a row to read.
//!
//! A fixed-literal insert with no computation, run once before the driver's
//! first batch. Re-running it before any real batch has committed is
//! harmless, since `batch_id = 0` stays the reset state either way, but this
//! is not a recovery step: it writes exactly one row, once, per fresh run.

use clickdoom_spec::RAM_BASE;
use clickhouse::Row;
use serde::Serialize;

use crate::client::{Db, Error};

/// `x1..x31`, SPEC's reset vector.
pub const RESET_REGS: [u32; 31] = [0; 31];

/// One `batch_commit` row's full shape: bootstrap's reset-state seed and a
/// benchmark's snapshot seed both write exactly this.
#[derive(Row, Serialize)]
pub(crate) struct BatchCommitRow {
    pub batch_id: u64,
    pub icount: u64,
    pub pc: u32,
    pub regs: Vec<u32>,
    pub halted: u8,
    pub halt_reason: String,
    pub exit_code: u32,
    pub keyq_pos: u64,
    pub has_frame: u8,
    pub frame_no: u32,
    pub wl_addr: Vec<u32>,
    pub wl_val: Vec<u32>,
    pub wl_icount: Vec<u64>,
    pub fb_wl_addr: Vec<u32>,
    pub fb_wl_val: Vec<u32>,
    pub fb_wl_icount: Vec<u64>,
    pub pal_wl_addr: Vec<u32>,
    pub pal_wl_val: Vec<u32>,
    pub pal_wl_icount: Vec<u64>,
    pub console_bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("regs holds {0} registers, x1..x31 needs exactly 31")]
    WrongRegisterCount(usize),
    #[error(transparent)]
    Db(#[from] Error),
}

/// What seeding did, for the caller to report.
#[derive(Debug)]
pub enum Seeded {
    /// A `batch_id = 0` row was written, with this many registers.
    Fresh { registers: usize },
    /// `batch_id = 0` already existed; nothing was written.
    AlreadySeeded,
}

/// Seeds `batch_id = 0` with `regs` (`x1..x31`), unless a `batch_id = 0` row
/// already exists.
pub async fn seed(db: &Db, regs: &[u32]) -> Result<Seeded, SeedError> {
    if regs.len() != 31 {
        return Err(SeedError::WrongRegisterCount(regs.len()));
    }

    let existing: u64 = db
        .fetch_one("SELECT count() FROM batch_commit WHERE batch_id = 0")
        .await?;
    if existing > 0 {
        return Ok(Seeded::AlreadySeeded);
    }

    let row = BatchCommitRow {
        batch_id: 0,
        icount: 0,
        pc: RAM_BASE,
        regs: regs.to_vec(),
        halted: 0,
        halt_reason: String::new(),
        exit_code: 0,
        keyq_pos: 0,
        has_frame: 0,
        frame_no: 0,
        wl_addr: Vec::new(),
        wl_val: Vec::new(),
        wl_icount: Vec::new(),
        fb_wl_addr: Vec::new(),
        fb_wl_val: Vec::new(),
        fb_wl_icount: Vec::new(),
        pal_wl_addr: Vec::new(),
        pal_wl_val: Vec::new(),
        pal_wl_icount: Vec::new(),
        console_bytes: Vec::new(),
    };
    db.insert_all("batch_commit", std::iter::once(row)).await?;

    Ok(Seeded::Fresh {
        registers: regs.len(),
    })
}
