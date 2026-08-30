//! What a run records about itself as it goes.
//!
//! Each of these serves a caller that used to step the machine in Python and
//! watch it from the inside. They are flags on one pass rather than
//! subcommands, because they all want the same image, the same budget and the
//! same stopping rule.

use std::collections::HashMap;

use clickdoom_spec::Region;

use crate::decode::Instruction;
use crate::exec::Cpu;

/// The counts as they stood somewhere the caller named.
pub struct Snapshot {
    pub label: String,
    /// Counts so far, ascending by program counter.
    pub rows: Vec<(u32, u64)>,
    /// Instructions retired by this point, which the rows sum to.
    pub retired: u64,
}

/// Retired instructions per program counter.
///
/// Dense over the read-only region, which is where a compiled program spends
/// nearly all of its time, and a map for anything outside it. One array
/// increment per instruction.
pub struct PcHistogram {
    base: u32,
    dense: Box<[u64]>,
    sparse: HashMap<u32, u64>,
    /// Named copies taken where the caller asked, so a window is the
    /// difference between two of them.
    pub snapshots: Vec<Snapshot>,
    retired: u64,
}

impl PcHistogram {
    pub fn new(text: Option<(u32, u32)>) -> Self {
        let (base, count) = match text {
            Some((start, end)) if end > start => (start, ((end - start) / 4) as usize),
            _ => (0, 0),
        };
        Self {
            base,
            dense: vec![0u64; count].into_boxed_slice(),
            sparse: HashMap::new(),
            snapshots: Vec::new(),
            retired: 0,
        }
    }

    #[inline]
    pub fn record(&mut self, pc: u32) {
        self.retired += 1;
        let delta = pc.wrapping_sub(self.base);
        if delta.is_multiple_of(4)
            && let Some(slot) = self.dense.get_mut((delta / 4) as usize)
        {
            *slot += 1;
            return;
        }
        *self.sparse.entry(pc).or_insert(0) += 1;
    }

    /// Counts so far, ascending by program counter, leaving out the ones that
    /// never ran.
    pub fn rows(&self) -> Vec<(u32, u64)> {
        let mut rows: Vec<(u32, u64)> = self
            .dense
            .iter()
            .enumerate()
            .filter(|(_, count)| **count > 0)
            .map(|(index, count)| (self.base + (index as u32) * 4, *count))
            .collect();
        rows.extend(self.sparse.iter().map(|(pc, count)| (*pc, *count)));
        rows.sort_by_key(|(pc, _)| *pc);
        rows
    }

    pub fn take_snapshot(&mut self, label: String) {
        let rows = self.rows();
        self.snapshots.push(Snapshot {
            label,
            rows,
            retired: self.retired,
        });
    }

    pub const fn retired(&self) -> u64 {
        self.retired
    }
}

/// A call to watch for, named.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TrapSpec {
    pub addr: u32,
    pub name: String,
}

impl std::str::FromStr for TrapSpec {
    type Err = String;

    /// `ADDR=NAME`, split at the last `=`, so a name may not contain one and
    /// an address may not need to.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (addr, name) = text
            .rsplit_once('=')
            .ok_or_else(|| format!("`{text}` is not ADDR=NAME"))?;
        Ok(Self {
            addr: super::point::parse_addr(addr)?,
            name: name.to_owned(),
        })
    }
}

/// One recorded call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TrapHit {
    /// The retired count before the trapped instruction runs, which is the
    /// only count available while the call's arguments are still in place.
    pub icount_before: u64,
    pub pc: u32,
    pub name_index: usize,
    pub regs: Vec<u32>,
}

/// Records a call whenever the program counter reaches a watched address.
///
/// A bit per instruction over the read-only region, so the check is a load
/// and a test rather than a hash lookup on every step.
pub struct Traps {
    base: u32,
    marked: Box<[u64]>,
    at: HashMap<u32, usize>,
    pub names: Vec<String>,
    pub regs: Vec<u8>,
    pub hits: Vec<TrapHit>,
    pub limit: u64,
    pub overflowed: bool,
}

impl Traps {
    pub fn new(specs: &[TrapSpec], regs: Vec<u8>, text: Option<(u32, u32)>, limit: u64) -> Self {
        let (base, count) = match text {
            Some((start, end)) if end > start => (start, ((end - start) / 4) as usize),
            _ => (0, 0),
        };
        let mut traps = Self {
            base,
            marked: vec![0u64; count.div_ceil(64)].into_boxed_slice(),
            at: HashMap::new(),
            names: Vec::new(),
            regs,
            hits: Vec::new(),
            limit,
            overflowed: false,
        };
        for spec in specs {
            let index = traps.names.len();
            traps.names.push(spec.name.clone());
            traps.at.insert(spec.addr, index);
            let delta = spec.addr.wrapping_sub(traps.base);
            if delta.is_multiple_of(4) {
                let word = (delta / 4) as usize;
                if let Some(slot) = traps.marked.get_mut(word / 64) {
                    *slot |= 1u64 << (word % 64);
                }
            }
        }
        traps
    }

    /// Whether this address might be watched. A false positive costs one map
    /// lookup; a false negative is impossible.
    #[inline]
    fn marked(&self, pc: u32) -> bool {
        let delta = pc.wrapping_sub(self.base);
        if !delta.is_multiple_of(4) {
            return !self.at.is_empty();
        }
        let word = (delta / 4) as usize;
        match self.marked.get(word / 64) {
            Some(slot) => *slot & (1u64 << (word % 64)) != 0,
            // Outside the dense range, fall back to the map.
            None => !self.at.is_empty(),
        }
    }

    #[inline]
    pub fn observe(&mut self, cpu: &Cpu) {
        if !self.marked(cpu.pc()) {
            return;
        }
        let Some(name_index) = self.at.get(&cpu.pc()).copied() else {
            return;
        };
        if self.hits.len() as u64 >= self.limit {
            self.overflowed = true;
            return;
        }
        self.hits.push(TrapHit {
            icount_before: cpu.icount(),
            pc: cpu.pc(),
            name_index,
            regs: self
                .regs
                .iter()
                .map(|index| cpu.regs()[*index as usize])
                .collect(),
        });
    }

    pub fn is_empty(&self) -> bool {
        self.at.is_empty()
    }
}

/// Everything a run may be asked to record.
#[derive(Default)]
pub struct Recorders {
    pub histogram: Option<PcHistogram>,
    pub traps: Option<Traps>,
    /// The regions to start watching, once the run reaches its window.
    pub watch_writes: Vec<Region>,
    /// Set once watching has started, so it starts once.
    pub watching: bool,
}

impl Recorders {
    #[inline]
    pub fn before(&mut self, cpu: &Cpu) {
        if let Some(traps) = &mut self.traps {
            traps.observe(cpu);
        }
    }

    #[inline]
    pub fn after(&mut self, retired_pc: u32, _insn: Instruction) {
        if let Some(histogram) = &mut self.histogram {
            histogram.record(retired_pc);
        }
    }

    pub const fn records_anything(&self) -> bool {
        self.histogram.is_some() || self.traps.is_some() || !self.watch_writes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickdoom_spec::RAM_BASE;

    #[test]
    fn a_histogram_counts_inside_and_outside_the_dense_range() {
        let mut hist = PcHistogram::new(Some((RAM_BASE, RAM_BASE + 16)));
        hist.record(RAM_BASE);
        hist.record(RAM_BASE);
        hist.record(RAM_BASE + 8);
        hist.record(0x1000_0000);
        assert_eq!(
            hist.rows(),
            vec![(0x1000_0000, 1), (RAM_BASE, 2), (RAM_BASE + 8, 1)]
        );
        assert_eq!(hist.retired(), 4);
    }

    #[test]
    fn a_histogram_over_no_declared_region_still_counts() {
        let mut hist = PcHistogram::new(None);
        hist.record(RAM_BASE);
        hist.record(RAM_BASE);
        assert_eq!(hist.rows(), vec![(RAM_BASE, 2)]);
    }

    #[test]
    fn the_sum_of_the_counts_is_what_retired() {
        let mut hist = PcHistogram::new(Some((RAM_BASE, RAM_BASE + 16)));
        for index in 0..10u32 {
            hist.record(RAM_BASE + (index % 4) * 4);
        }
        let total: u64 = hist.rows().iter().map(|(_, count)| count).sum();
        assert_eq!(total, hist.retired());
        assert_eq!(total, 10);
    }

    #[test]
    fn a_snapshot_is_the_counts_as_they_stood() {
        let mut hist = PcHistogram::new(Some((RAM_BASE, RAM_BASE + 16)));
        hist.record(RAM_BASE);
        hist.take_snapshot("first".to_owned());
        hist.record(RAM_BASE);
        hist.take_snapshot("second".to_owned());
        assert_eq!(hist.snapshots[0].rows, vec![(RAM_BASE, 1)]);
        assert_eq!(hist.snapshots[1].rows, vec![(RAM_BASE, 2)]);
        assert_eq!(hist.snapshots[0].retired, 1);
    }

    #[test]
    fn a_trap_spec_splits_at_the_last_equals() {
        let spec: TrapSpec = "0x80001234=memcpy".parse().unwrap();
        assert_eq!(spec.addr, 0x8000_1234);
        assert_eq!(spec.name, "memcpy");
        assert!("nothing".parse::<TrapSpec>().is_err());
    }

    #[test]
    fn a_watched_address_is_found_and_an_unwatched_one_is_not() {
        let specs = vec![
            TrapSpec {
                addr: RAM_BASE + 8,
                name: "memcpy".to_owned(),
            },
            TrapSpec {
                addr: 0x1000_0000,
                name: "outside".to_owned(),
            },
        ];
        let traps = Traps::new(
            &specs,
            vec![10, 11, 12],
            Some((RAM_BASE, RAM_BASE + 64)),
            100,
        );
        assert!(traps.marked(RAM_BASE + 8));
        assert!(!traps.marked(RAM_BASE + 4));
        // Outside the dense range the map decides, so it is still found.
        assert!(traps.marked(0x1000_0000));
    }

    #[test]
    fn passing_the_limit_is_recorded_rather_than_truncating_quietly() {
        let specs = vec![TrapSpec {
            addr: RAM_BASE,
            name: "f".to_owned(),
        }];
        let mut traps = Traps::new(&specs, vec![10], Some((RAM_BASE, RAM_BASE + 8)), 2);
        let cpu = Cpu::inert();
        for _ in 0..5 {
            traps.observe(&cpu);
        }
        assert_eq!(traps.hits.len(), 2);
        assert!(traps.overflowed);
    }
}
