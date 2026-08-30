//! The row shape `executor::fold::select_only` and `batch`'s underlying
//! fold produce, for a caller that needs to read the result directly rather
//! than let `batch`'s own INSERT commit it.

use clickhouse::Row;
use serde::Deserialize;

#[derive(Row, Deserialize, Debug)]
pub struct FoldResult {
    pub pc: u32,
    pub regs: Vec<u32>,
    pub wl_addr: Vec<u32>,
    pub wl_val: Vec<u32>,
    pub wl_icount: Vec<u64>,
    pub stopped: u8,
    pub halted: u8,
    pub halt_reason: u8,
    pub halt_pc: u32,
    pub halt_extra: u32,
    pub retired: u32,
    pub console_bytes: Vec<u8>,
    pub keyq_pos: u32,
    pub frame_no: u32,
    pub frame_committed: u8,
    pub fb_wl_addr: Vec<u32>,
    pub fb_wl_val: Vec<u32>,
    pub fb_wl_icount: Vec<u64>,
    pub pal_wl_addr: Vec<u32>,
    pub pal_wl_val: Vec<u32>,
    pub pal_wl_icount: Vec<u64>,
}
