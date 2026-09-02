//! Every call to the engine's random-number function, with its caller.
//!
//! A state divergence says which column moved. This says which action
//! function asked for the number that moved it, which is the step from "the
//! two sides disagree" to "they disagree in `P_MobjThinker`".
//!
//! One row per call, carrying the tic it happened in and the call's position
//! within that tic. Grouping by `gametic` gives the sequence of calls the tic
//! made, which is what the two sides have to agree on.

use std::fmt::Write as _;
use std::io::Write;

use crate::decode::Instruction;
use crate::exec::Cpu;
use crate::image::{Image, Symbol, function_containing};
use crate::trace::{Observer, Step};

use super::ProbeError;
use super::ram::Ram;
use super::world::{self, Engine};

/// The register holding the return address, which is where the call came
/// from. `ra` is `x1` in the RISC-V calling convention.
const RA: usize = 1;

/// The function whose calls are logged.
pub const RANDOM_FUNCTION: &str = "P_Random";

/// The header a random-call file carries.
pub fn header() -> String {
    "# refemu-probe-rng 1\n# columns\tgametic\tcall_index\tcaller\tcaller_offset\ticount\n"
        .to_owned()
}

/// Logs a call to one function every time the program counter reaches it.
///
/// The check is one compare against the program counter per retired
/// instruction. Reading the tic and naming the caller cost nothing until a
/// call lands.
pub struct RngLog<W: Write> {
    at: u32,
    gametic: u32,
    functions: Vec<Symbol>,
    out: W,
    tic: Option<i32>,
    index: u64,
    pub rows: u64,
    /// The first error, which stops the run.
    pub failed: Option<ProbeError>,
}

impl<W: Write> RngLog<W> {
    /// Resolves the watched function and the tic counter, so a run that
    /// cannot log fails before it starts.
    pub fn new(image: &Image, engine: &Engine, out: W) -> Result<Self, ProbeError> {
        let at = image
            .symbol(RANDOM_FUNCTION)
            .map(|s| s.addr)
            .ok_or_else(|| ProbeError::NoSymbol(RANDOM_FUNCTION.to_owned()))?;
        Ok(Self {
            at,
            gametic: engine.globals.gametic,
            functions: image.functions(),
            out,
            tic: None,
            index: 0,
            rows: 0,
            failed: None,
        })
    }

    pub fn into_sink(self) -> W {
        self.out
    }

    fn record(&mut self, cpu: &Cpu) -> Result<(), ProbeError> {
        let ram: Ram<'_> = world::ram_of(cpu);
        let tic = ram.i32(self.gametic, "gametic")?;
        if self.tic != Some(tic) {
            self.tic = Some(tic);
            self.index = 0;
        }
        let ra = cpu.regs()[RA];
        let caller =
            function_containing(&self.functions, ra).ok_or(ProbeError::UnknownCaller { ra })?;

        let mut row = String::with_capacity(64);
        let _ = writeln!(
            row,
            "{tic}\t{}\t{}\t{}\t{}",
            self.index,
            caller.name,
            ra - caller.addr,
            cpu.icount()
        );
        self.out
            .write_all(row.as_bytes())
            .map_err(|e| ProbeError::Write(e.to_string()))?;
        self.index += 1;
        self.rows += 1;
        Ok(())
    }
}

impl<W: Write> Observer for RngLog<W> {
    /// Before the call's first instruction runs, which is while `ra` still
    /// holds the address the call will return to.
    fn before_step(&mut self, cpu: &mut Cpu) -> Step {
        if cpu.pc() != self.at {
            return Step::Continue;
        }
        if let Err(error) = self.record(cpu) {
            self.failed = Some(error);
            return Step::Stop;
        }
        Step::Continue
    }

    fn after_step(&mut self, _cpu: &mut Cpu, _retired_pc: u32, _insn: Instruction) -> Step {
        Step::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_names_the_columns_the_rows_carry() {
        let header = header();
        let columns: Vec<&str> = header
            .lines()
            .find_map(|line| line.strip_prefix("# columns\t"))
            .unwrap()
            .split('\t')
            .collect();
        assert_eq!(
            columns,
            ["gametic", "call_index", "caller", "caller_offset", "icount"]
        );
    }
}
