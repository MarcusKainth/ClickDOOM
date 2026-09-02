//! DOOM's game state, read out of the machine's RAM at every frame commit.
//!
//! The native-mode SQL simulation writes one row per tic. This writes rows of
//! the same shape from the real engine, so the two can be compared column by
//! column. `clickdoom_spec::native_state` owns the field list and its order,
//! and both writers read it from there.
//!
//! Nothing here changes the run. The probe reads the RAM slice directly rather
//! than through `Memory::read`, which would pop the key queue, and it never
//! writes.
//!
//! `README.md` beside this module says what each column holds and what the
//! parity query has to ignore.

pub mod layout;
pub mod ram;
pub mod row;
pub mod world;

use std::io::Write;

use clickdoom_spec::native_state::STATE_SCHEMA_VERSION;

use crate::decode::Instruction;
use crate::exec::Cpu;
use crate::image::Image;
use crate::trace::{Observer, Step};

pub use layout::Layout;
pub use world::{Engine, ThinkerKind};

/// Anything that stops the probe reading a frame.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("layout.tsv:{line}: expected {what}")]
    LayoutRow { line: usize, what: &'static str },
    #[error("layout.tsv holds no rows")]
    LayoutEmpty,
    #[error("layout.tsv has no {type_name}.{field}")]
    LayoutMissing { type_name: String, field: String },
    #[error("layout.tsv makes {type_name}.{field} {got} bytes, and the probe reads {want}")]
    LayoutWidth {
        type_name: String,
        field: String,
        want: u32,
        got: u32,
    },
    #[error("the image has no symbol named {0}")]
    NoSymbol(String),
    #[error("the image has more than one symbol named {0}, so nothing says which is meant")]
    AmbiguousSymbol(String),
    #[error("{name} is {got} bytes and the probe reads {want}")]
    SymbolSize { name: String, want: u32, got: u32 },
    #[error("{what} is at {addr:#010x}, which is {len} bytes outside RAM")]
    OutsideRam {
        addr: u32,
        len: u32,
        what: &'static str,
    },
    #[error("the thinker at {addr:#010x} runs {function:#010x}, which names no known thinker")]
    UnknownThinker { addr: u32, function: u32 },
    #[error("the thinker list did not return to its head within {0} entries")]
    ThinkerListRuns(u32),
    #[error("the thinker at {0:#010x} is a mobj where a sector thinker was expected")]
    MobjAsSectorThinker(u32),
    #[error("{what} at {addr:#010x} points at {value:#010x}, which is not a {into}")]
    NotAnIndex {
        what: &'static str,
        addr: u32,
        value: u32,
        into: &'static str,
    },
    #[error("wrote {wrote} for the column named {want}")]
    ColumnOutOfOrder { wrote: String, want: String },
    #[error("the row stopped after {wrote} of {total} columns")]
    RowTooShort { wrote: usize, total: usize },
    #[error("writing a row: {0}")]
    Write(String),
}

/// Which frames a run writes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Frames(Vec<std::ops::RangeInclusive<u64>>);

impl Frames {
    /// Every frame, which is what a run with no selection writes.
    pub fn all() -> Self {
        Self(Vec::new())
    }

    pub fn contains(&self, index: u64) -> bool {
        self.0.is_empty() || self.0.iter().any(|range| range.contains(&index))
    }

    /// The last frame any range names, or `None` when every frame is wanted.
    pub fn last(&self) -> Option<u64> {
        (!self.0.is_empty())
            .then(|| self.0.iter().map(|range| *range.end()).max())
            .flatten()
    }
}

impl std::str::FromStr for Frames {
    type Err = String;

    /// `0,1,37..45`, where each part is an index or an inclusive range.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut ranges = Vec::new();
        for part in text.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (first, last) = match part.split_once("..") {
                Some((first, last)) => (first, last),
                None => (part, part),
            };
            let first = crate::cli::point::parse_count(first)?;
            let last = crate::cli::point::parse_count(last)?;
            if last < first {
                return Err(format!("`{part}` ends before it starts"));
            }
            ranges.push(first..=last);
        }
        if ranges.is_empty() {
            return Err("no frame is named".to_owned());
        }
        Ok(Self(ranges))
    }
}

/// What a probe run recorded about itself.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Written {
    pub rows: u64,
    pub bytes: u64,
    /// Frame commits the run saw, whether or not their rows were written.
    pub frames_seen: u64,
    /// The first frame index whose gametic did not repeat the frame before
    /// it, after the last one that did. Under `-timedemo` the screen melt
    /// commits many frames within one tic, and gameplay is what follows.
    pub first_gameplay_frame: Option<u64>,
    last_gametic: Option<i32>,
    last_repeat: Option<u64>,
}

/// The header a probe file carries, so a reader can tell what it is holding.
pub fn header() -> String {
    let mut columns = vec![
        "frame_index".to_owned(),
        "gametic".to_owned(),
        "fb_hash".to_owned(),
    ];
    columns.extend(
        clickdoom_spec::native_state::all_fields()
            .into_iter()
            .map(str::to_owned),
    );
    format!(
        "# refemu-probe 1\n# state_schema_version\t{STATE_SCHEMA_VERSION}\n# columns\t{}\n",
        columns.join("\t")
    )
}

/// Writes one row per frame commit, for as long as the run goes.
///
/// The observer only looks at the frame-commit log, which is a length compare
/// per retired instruction. Reading the state costs nothing until a frame
/// lands.
pub struct Probe<W: Write> {
    engine: Engine,
    frames: Frames,
    out: W,
    seen: u64,
    pub written: Written,
    /// The first error, which stops the run. A probe that carried on past one
    /// would write a file with a hole nobody could see.
    pub failed: Option<ProbeError>,
}

impl<W: Write> Probe<W> {
    /// Resolves everything the probe needs from the image and the table, so a
    /// run that cannot read the state fails before it starts.
    pub fn new(image: &Image, layout: &Layout, frames: Frames, out: W) -> Result<Self, ProbeError> {
        Ok(Self {
            engine: Engine::resolve(image, layout)?,
            frames,
            out,
            seen: 0,
            written: Written::default(),
            failed: None,
        })
    }

    /// The sink, once the run is over.
    pub fn into_sink(self) -> W {
        self.out
    }

    /// Whether every frame the selection names has been written, which is when
    /// a selected run has nothing left to do.
    pub fn done(&self) -> bool {
        self.frames.last().is_some_and(|last| self.seen > last)
    }

    fn dump(&mut self, cpu: &Cpu) -> Result<(), ProbeError> {
        let index = self.seen - 1;
        let text = row::write(&self.engine, cpu, index)?;
        self.out
            .write_all(text.as_bytes())
            .map_err(|e| ProbeError::Write(e.to_string()))?;
        self.written.rows += 1;
        self.written.bytes += text.len() as u64;
        Ok(())
    }
}

impl<W: Write> Observer for Probe<W> {
    fn after_step(&mut self, cpu: &mut Cpu, _retired_pc: u32, _insn: Instruction) -> Step {
        let commits = cpu
            .memory
            .devices()
            .registers_ref()
            .map_or(0, |r| r.frame_commits.len() as u64);
        if commits == self.seen {
            return Step::Continue;
        }
        self.seen = commits;
        self.written.frames_seen = commits;
        let index = commits - 1;

        match world::gametic(&self.engine, cpu) {
            Ok(gametic) => {
                if self.written.last_gametic == Some(gametic) {
                    self.written.last_repeat = Some(index);
                }
                self.written.last_gametic = Some(gametic);
                self.written.first_gameplay_frame =
                    Some(self.written.last_repeat.map_or(0, |at| at + 1));
            }
            Err(error) => {
                self.failed = Some(error);
                return Step::Stop;
            }
        }

        if self.frames.contains(index)
            && let Err(error) = self.dump(cpu)
        {
            self.failed = Some(error);
            return Step::Stop;
        }
        if self.done() {
            return Step::Stop;
        }
        Step::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_selection_takes_indices_and_ranges() {
        let frames: Frames = "0,1,37..45".parse().unwrap();
        for index in [0, 1, 37, 40, 45] {
            assert!(frames.contains(index), "{index}");
        }
        for index in [2, 36, 46, 1000] {
            assert!(!frames.contains(index), "{index}");
        }
        assert_eq!(frames.last(), Some(45));
    }

    #[test]
    fn no_selection_means_every_frame_and_never_finishes_early() {
        let frames = Frames::all();
        assert!(frames.contains(0));
        assert!(frames.contains(u64::MAX));
        assert_eq!(frames.last(), None);
    }

    #[test]
    fn a_selection_that_names_nothing_is_an_error() {
        assert!("".parse::<Frames>().is_err());
        assert!("45..37".parse::<Frames>().is_err());
        assert!("one".parse::<Frames>().is_err());
    }

    #[test]
    fn the_header_names_every_contract_column_in_order() {
        let header = header();
        let columns: Vec<&str> = header
            .lines()
            .find_map(|line| line.strip_prefix("# columns\t"))
            .unwrap()
            .split('\t')
            .collect();
        assert_eq!(&columns[..3], &["frame_index", "gametic", "fb_hash"]);
        assert_eq!(
            &columns[3..],
            clickdoom_spec::native_state::all_fields().as_slice()
        );
    }
}
