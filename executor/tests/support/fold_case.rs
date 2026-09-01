//! Running one hand-built instruction stream through
//! [`clickdoom_executor::fold::select_only`], and the row it comes back as.
//!
//! The decode table is padded up to a power of two with ILLEGAL rows, so a
//! stray jump target lands on something that halts rather than on whatever
//! the table happens to end with. The oracle is given the same padded
//! table.
//!
//! A case that needs a register seeded uses a leading `addi` rather than
//! `select_only`'s `regs0`. A literal array of small values infers as
//! `Array(UInt8)`, the accumulator is `Array(UInt32)`, and `arrayFold`
//! rejects the whole query on a type mismatch before a single instruction
//! runs.

use clickdoom_executor::fold::{self, SelectOnlyArgs, Variant};
use clickhouse::Row;
use serde::Deserialize;

use super::RAM_BASE;
use super::db::Db;
use super::fixture::Fixture;
use super::reference::{self, Insn, Outcome};

/// One case: the instruction stream, the memory it starts from, and the
/// knobs `select_only` takes.
pub struct FoldCase<'a> {
    pub insns: &'a [Insn],
    /// Word index relative to RAM_BASE, and the value there. Every other
    /// word in the window starts at 0.
    pub ram: &'a [(u32, u32)],
    /// Words of RAM the fixture spans. Defaults to the padded decode
    /// table's length, which puts every RAM word inside the text window.
    pub ram_words: Option<u32>,
    /// Instructions the batch is asked for. Defaults to the number of
    /// instructions the case supplied, before padding.
    pub k: Option<u32>,
    pub hwm: u32,
    pub icount0: u64,
    pub keyq_events: &'a [u16],
    pub wl0: &'a str,
}

impl Default for FoldCase<'_> {
    fn default() -> Self {
        FoldCase {
            insns: &[],
            ram: &[],
            ram_words: None,
            k: None,
            hwm: 10_000,
            icount0: 0,
            keyq_events: &[],
            wl0: fold::WL0_EMPTY,
        }
    }
}

/// Every column `select_only` projects.
#[derive(Row, Deserialize, Debug)]
pub struct FoldRow {
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

impl FoldRow {
    /// Register `n` of x1..x31. `regs` has no slot for x0.
    pub fn x(&self, n: usize) -> u32 {
        assert!((1..=31).contains(&n), "x{n} is not one of x1..x31");
        self.regs[n - 1]
    }

    /// The fields the reference model also produces, for comparison.
    pub fn outcome(&self) -> Outcome {
        let regs: [u32; 31] = self
            .regs
            .as_slice()
            .try_into()
            .expect("regs is x1..x31, 31 elements");
        Outcome {
            pc: self.pc,
            regs,
            wl_addr: self.wl_addr.clone(),
            wl_val: self.wl_val.clone(),
            wl_icount: self.wl_icount.clone(),
            stopped: self.stopped,
            halted: self.halted,
            halt_reason: self.halt_reason,
            halt_pc: self.halt_pc,
            halt_extra: self.halt_extra,
            retired: self.retired,
        }
    }
}

/// The padded decode table and the RAM window a case resolves to.
struct Prepared {
    insns: Vec<Insn>,
    decn: u32,
    ram_words: u32,
    ram: Vec<u32>,
    k: u32,
}

fn prepare(case: &FoldCase<'_>) -> Prepared {
    let decn = (case.insns.len().max(8)).next_power_of_two() as u32;
    let mut insns = case.insns.to_vec();
    for index in insns.len()..decn as usize {
        insns.push(Insn {
            op_id: reference::OP_ILLEGAL,
            raw: 0xBAD0_0000 + index as u32,
            ..Insn::default()
        });
    }
    let ram_words = case.ram_words.unwrap_or(decn);
    assert!(
        ram_words >= decn,
        "ram_words={ram_words} must contain the text window of {decn} words"
    );
    let mut ram = vec![0u32; ram_words as usize];
    for (widx, value) in case.ram {
        ram[*widx as usize] = *value;
    }
    Prepared {
        insns,
        decn,
        ram_words,
        ram,
        k: case.k.unwrap_or(case.insns.len() as u32),
    }
}

/// The step formulation every case runs against, named by
/// `CLICKDOOM_STEP_VARIANT` in the kebab-case spelling
/// `examples/step_variants.rs` takes. Unset is the shipped one, so an
/// ordinary run covers what production executes. Setting it reruns this
/// whole suite, reference comparison included, against one other arm.
fn variant() -> Variant {
    let name = match std::env::var("CLICKDOOM_STEP_VARIANT") {
        Ok(name) if !name.is_empty() => name,
        _ => return Variant::Baseline,
    };
    let variant = match name.as_str() {
        "baseline" => Variant::Baseline,
        "inline-halt-code" => Variant::InlineHaltCode,
        "short-binding-param" => Variant::ShortBindingParam,
        "bind-decode-row" => Variant::BindDecodeRow,
        "bind-repeated" => Variant::BindRepeated,
        "fewer-constants" => Variant::FewerConstants,
        "more-constants" => Variant::MoreConstants,
        other => panic!("CLICKDOOM_STEP_VARIANT={other:?} is not a step variant"),
    };
    println!("CLICKDOOM_STEP_VARIANT={name}: running against {variant:?}");
    variant
}

async fn execute(db: &Db, fx: &Fixture, case: &FoldCase<'_>, p: &Prepared) -> FoldRow {
    fx.truncate(&["decoded", "ram", "input_queue"]).await;
    fx.seed_decoded(&p.insns).await;
    fx.seed_ram(&p.ram).await;
    fx.seed_input_queue(case.keyq_events).await;
    let sql = fold::select_only_variant(
        p.k,
        0,
        p.decn,
        p.decn,
        p.ram_words,
        case.hwm,
        &SelectOnlyArgs {
            db: &fx.database,
            icount0: case.icount0,
            wl0: case.wl0,
            ..Default::default()
        },
        variant(),
    );
    db.fetch_one::<FoldRow>(&sql).await.unwrap()
}

fn model(p: &Prepared, hwm: u32) -> Outcome {
    reference::run(&reference::Run {
        insns: &p.insns,
        ram_base: RAM_BASE,
        ram_words: p.ram_words,
        text_start_widx: 0,
        text_end_widx: p.decn,
        ram0: &p.ram,
        regs0: [0; 31],
        pc0: RAM_BASE,
        k: p.k,
        hwm,
    })
}

/// Runs the case and requires the fold to agree with the reference model,
/// returning the fold's own row so a test can assert on it further.
pub async fn run_checked(fx: &Fixture, case: &FoldCase<'_>) -> FoldRow {
    run_checked_labelled(fx, case, "").await
}

/// [`run_checked`] with a label naming which case in a loop failed.
pub async fn run_checked_labelled(fx: &Fixture, case: &FoldCase<'_>, label: &str) -> FoldRow {
    checked(&fx.db, fx, case, label).await
}

/// [`run_checked`] with the query issued through `db` rather than the
/// fixture's own client, for a case whose session has to ask the server
/// for something the fold's own `SETTINGS` clause overrides.
pub async fn run_checked_through(db: &Db, fx: &Fixture, case: &FoldCase<'_>) -> FoldRow {
    checked(db, fx, case, "").await
}

async fn checked(db: &Db, fx: &Fixture, case: &FoldCase<'_>, label: &str) -> FoldRow {
    let p = prepare(case);
    let row = execute(db, fx, case, &p).await;
    assert_eq!(
        row.outcome(),
        model(&p, case.hwm),
        "the fold and the reference model disagree{}{label}",
        if label.is_empty() { "" } else { ": " }
    );
    row
}

/// Runs the case without the reference comparison, for the cases whose
/// addresses fall in a region the model does not carry: MMIO, FRAMEBUFFER
/// and PALETTE.
pub async fn run_raw(fx: &Fixture, case: &FoldCase<'_>) -> FoldRow {
    let p = prepare(case);
    execute(&fx.db, fx, case, &p).await
}
