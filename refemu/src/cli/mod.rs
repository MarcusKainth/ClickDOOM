//! The command line.
//!
//! A subcommand owns a contract: the bytes it writes and where it writes them.
//! A flag owns an observation, which is one more thing to record about the
//! same pass over the same program. Profiling is a set of flags rather than a
//! subcommand, because it needs every option `run` already has and would
//! otherwise restate them and then drift.

pub mod observe;
pub mod point;
pub mod report;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clickdoom_spec::{
    Checkpoint, HaltReason, IPMS_DEFAULT, Manifest, MemoryMap, RAM_BASE, RAM_SIZE, Region,
    Sha256Stream, TraceConfig, assert_pinned_hash, sha256_hex,
};

use crate::boot::{RETIRED_PC_HISTORY, format_report};
use crate::decode::decode;
use crate::exec::{Cpu, Halt};
use crate::image::{Image, read_image};
use crate::memory::Memory;
use crate::mmio::Devices;
use crate::snapshot::{self, Provenance, Snapshot};
use crate::trace::{self, Observer, Step, Stop};
use observe::{
    ConsoleMilestone, MilestoneSpec, NamedCount, PcHistogram, Recorders, TrapSpec, Traps,
};
use point::{Point, StopAt, WatchFrom, parse_addr, parse_count, parse_hash64};
use report::{
    FinalState, FrameCommitJson, HaltJson, Milestone, RunOutcome, RunReport, Timing, TraceMeta,
    write_json,
};

/// What the process reports.
///
/// One scheme across every subcommand. Two of the Python's are folded into
/// this: a halt is its own code rather than sharing one with a missing file,
/// and running out of budget is separable from reaching a stop.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Exit {
    /// Reached a stop the caller asked for, and every check passed.
    Ok = 0,
    /// Could not do the job. A file is missing, a path is unwritable.
    Failed = 1,
    /// The command line is wrong. This is what clap already uses.
    Usage = 2,
    /// The machine halted and no stop named a halt.
    Halted = 3,
    /// The budget ran out and no stop named the budget.
    Budget = 4,
    /// An expectation or a pinned hash did not hold.
    Gate = 5,
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        ExitCode::from(exit as u8)
    }
}

#[derive(Parser)]
#[command(
    name = "refemu",
    version,
    about = "An RV32IM interpreter. It's the oracle the SQL CPU is checked against.",
    after_help = AFTER_HELP
)]
pub struct Cli {
    /// Print errors only
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Say what each step of a run recorded
    #[arg(short, long, global = true)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Command,
}

const AFTER_HELP: &str = "\
Every command reads a flat binary. A command's data goes to stdout, so a shell
can redirect it. Progress and diagnostics go to stderr.

Exit codes:
  0  reached the stop you asked for
  1  refemu couldn't do the job, because a file it needs is missing or bad
  2  the command line is wrong
  3  the machine halted and you didn't ask to stop at a halt
  4  the run hit --max-instructions before any stop condition
  5  an --expect check or --pinned-hash failed";

#[derive(Subcommand)]
pub enum Command {
    /// Run an image and report what happened
    Run(RunCmd),
    /// Run an image and emit the checkpoint trace
    Trace(TraceCmd),
    /// Run to the first announced frame and report what happened
    Boot(BootCmd),
    /// Decode instructions without running them
    Disasm(DisasmCmd),
    /// Print a checkpoint hash over bytes you supply
    Hash(HashCmd),
}

#[derive(Args, Clone)]
pub struct ImageArgs {
    /// Flat binary
    pub image: PathBuf,
    /// Manifest holding load_addr, text_start and text_end
    #[arg(long, value_name = "PATH")]
    pub manifest: Option<PathBuf>,
    /// Don't look for a manifest next to IMAGE
    #[arg(long, conflicts_with = "manifest")]
    pub no_manifest: bool,
    /// How to read IMAGE
    #[arg(long, value_enum, default_value_t = ImageFormat::Auto)]
    pub format: ImageFormat,
    /// Where execution starts. Defaults to the image's own entry
    #[arg(long, value_name = "ADDR", value_parser = parse_addr)]
    pub entry: Option<u32>,
    /// Where the image loads
    #[arg(long, value_name = "ADDR", value_parser = parse_addr)]
    pub load_addr: Option<u32>,
    /// Start of the read-only text region
    #[arg(long, value_name = "ADDR", value_parser = parse_addr)]
    pub text_start: Option<u32>,
    /// End of the read-only text region
    #[arg(long, value_name = "ADDR", value_parser = parse_addr)]
    pub text_end: Option<u32>,
    /// Refuse to run unless the image's sha256 matches this file
    #[arg(long, value_name = "PATH")]
    pub pinned_hash: Option<PathBuf>,
}

#[derive(Args, Clone)]
pub struct MachineArgs {
    /// RAM size in bytes
    #[arg(long, value_name = "BYTES", default_value_t = RAM_SIZE, value_parser = parse_addr)]
    pub ram_size: u32,
    /// Instructions per emulated millisecond
    #[arg(long, value_name = "N", default_value_t = IPMS_DEFAULT)]
    pub ipms: u32,
    /// Device behaviour
    #[arg(long, value_enum, default_value_t = DeviceSet::Full)]
    pub devices: DeviceSet,
}

/// How to read the image file.
#[derive(Copy, Clone, PartialEq, Eq, Debug, ValueEnum)]
pub enum ImageFormat {
    /// An ELF if it starts with one, a flat binary otherwise.
    Auto,
    /// The whole file at one address.
    Flat,
    /// Loadable segments where the file says they go.
    Elf,
}

/// Which device model the window presents.
#[derive(Copy, Clone, PartialEq, Eq, Debug, ValueEnum)]
pub enum DeviceSet {
    /// The five registers.
    Full,
    /// Plain byte storage, which is what the riscv-tests fixtures need.
    None,
}

#[derive(Args, Clone)]
pub struct BudgetArgs {
    /// Instruction budget
    #[arg(long, short = 'n', value_name = "N", default_value_t = 10_000_000, value_parser = parse_count)]
    pub max_instructions: u64,
    /// Stop here. Repeatable, and the earliest one wins
    #[arg(long = "stop-at", value_name = "COND")]
    pub stop_at: Vec<StopAt>,
}

impl BudgetArgs {
    /// With nothing named, running out of budget is the ending the caller
    /// asked for.
    fn conditions(&self) -> Vec<Point> {
        if self.stop_at.is_empty() {
            vec![Point::Budget]
        } else {
            self.stop_at.iter().map(|s| s.0).collect()
        }
    }
}

#[derive(Args, Clone, Default)]
pub struct ExpectArgs {
    /// Fail unless the run stops at this count
    #[arg(long, value_name = "N", value_parser = parse_count)]
    pub expect_icount: Option<u64>,
    /// Fail unless the register hash matches at the stop
    #[arg(long, value_name = "HEX", value_parser = parse_hash64)]
    pub expect_reghash: Option<u64>,
    /// Fail unless the RAM hash matches at the stop
    #[arg(long, value_name = "HEX", value_parser = parse_hash64)]
    pub expect_ramhash: Option<u64>,
    /// Fail unless the framebuffer hash matches at the stop
    #[arg(long, value_name = "HEX", value_parser = parse_hash64)]
    pub expect_fbhash: Option<u64>,
}

#[derive(Args, Clone)]
pub struct RunCmd {
    #[command(flatten)]
    pub image: ImageArgs,
    #[command(flatten)]
    pub machine: MachineArgs,
    #[command(flatten)]
    pub budget: BudgetArgs,
    #[command(flatten)]
    pub expect: ExpectArgs,
    /// Write the run outcome as JSON
    #[arg(long, value_name = "PATH")]
    pub halt_report: Option<PathBuf>,
    /// Write the bytes the program printed
    #[arg(long, value_name = "PATH")]
    pub console_out: Option<PathBuf>,
    #[command(flatten)]
    pub capture: CaptureArgs,
    #[command(flatten)]
    pub observe: ObserveArgs,
}

/// What a run records while it goes.
#[derive(Args, Clone, Default)]
pub struct ObserveArgs {
    /// Start recording here
    #[arg(long, value_name = "POINT", default_value = "start")]
    pub watch_from: Option<WatchFrom>,
    /// Write retired-instruction counts per program counter
    #[arg(long, value_name = "PATH")]
    pub pc_histogram: Option<PathBuf>,
    /// Take a histogram snapshot here. Repeatable
    #[arg(long, value_name = "POINT", requires = "pc_histogram")]
    pub histogram_at: Vec<WatchFrom>,
    /// Record a call whenever the program counter reaches ADDR. Repeatable
    #[arg(long, value_name = "ADDR=NAME")]
    pub trap_pc: Vec<TrapSpec>,
    /// Read ADDR and NAME pairs from a file, one per line
    #[arg(long, value_name = "PATH")]
    pub trap_pcs: Option<PathBuf>,
    /// Registers a trap records
    #[arg(
        long,
        value_name = "REGS",
        value_delimiter = ',',
        default_value = "10,11,12"
    )]
    pub trap_regs: Vec<u8>,
    /// Write the trap call log
    #[arg(long, value_name = "PATH")]
    pub trap_report: Option<PathBuf>,
    /// Fail once the trap log passes this many rows
    #[arg(long, value_name = "N", default_value_t = 5_000_000, value_parser = parse_count)]
    pub trap_limit: u64,
    /// Record which words these regions write
    #[arg(long, value_name = "REGIONS", value_delimiter = ',')]
    pub watch_writes: Vec<WatchRegion>,
    /// Write the write-coverage report
    #[arg(long, value_name = "PATH", requires = "watch_writes")]
    pub write_coverage: Option<PathBuf>,
    /// Fail unless every word of these regions is written in the window
    #[arg(
        long,
        value_name = "REGIONS",
        value_delimiter = ',',
        requires = "watch_writes"
    )]
    pub require_coverage: Vec<WatchRegion>,
    /// Write one row per announced frame
    #[arg(long, value_name = "PATH")]
    pub frame_log: Option<PathBuf>,
}

/// A region a run can watch writes to.
#[derive(Copy, Clone, PartialEq, Eq, Debug, ValueEnum)]
pub enum WatchRegion {
    Ram,
    #[value(name = "fb", alias = "framebuffer")]
    Framebuffer,
    #[value(name = "pal", alias = "palette")]
    Palette,
}

impl WatchRegion {
    const fn region(self) -> Region {
        match self {
            WatchRegion::Ram => Region::Ram,
            WatchRegion::Framebuffer => Region::Framebuffer,
            WatchRegion::Palette => Region::Palette,
        }
    }
}

/// What a run keeps of the machine it leaves behind.
#[derive(Args, Clone, Default)]
pub struct CaptureArgs {
    /// Write the whole machine at the stop
    #[arg(long, value_name = "PATH")]
    pub dump_state: Option<PathBuf>,
    /// Write the framebuffer and palette at the stop
    #[arg(long, value_name = "PATH")]
    pub dump_frame: Option<PathBuf>,
    /// Start from a captured machine instead of the image's entry
    #[arg(long, value_name = "PATH")]
    pub resume: Option<PathBuf>,
    /// Resume even from a machine captured under different settings
    #[arg(long, requires = "resume")]
    pub force_resume: bool,
}

#[derive(Args, Clone)]
pub struct TraceCmd {
    #[command(flatten)]
    pub image: ImageArgs,
    #[command(flatten)]
    pub machine: MachineArgs,
    #[command(flatten)]
    pub budget: BudgetArgs,
    #[command(flatten)]
    pub expect: ExpectArgs,
    /// Write the trace here instead of stdout
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
    /// Write the run outcome as JSON
    #[arg(long, value_name = "PATH")]
    pub halt_report: Option<PathBuf>,
    /// Write the bytes the program printed
    #[arg(long, value_name = "PATH")]
    pub console_out: Option<PathBuf>,
    /// Instructions between checkpoints
    #[arg(long, value_name = "N", default_value_t = TraceConfig::default().checkpoint_interval, value_parser = parse_count)]
    pub checkpoint_interval: u64,
    /// Instructions between the memory hashes
    #[arg(long, value_name = "N", default_value_t = TraceConfig::default().ram_hash_interval, value_parser = parse_count)]
    pub ram_hash_interval: u64,
    /// Write the trace into this directory, named after the image's sha256
    #[arg(long, value_name = "DIR", requires = "name", conflicts_with = "out")]
    pub out_dir: Option<PathBuf>,
    /// Filename stem used with --out-dir
    #[arg(long, value_name = "STEM")]
    pub name: Option<String>,
    /// Write the trace metadata here. With --out-dir it defaults beside the trace
    #[arg(long, value_name = "PATH")]
    pub meta_out: Option<PathBuf>,
    /// Name the count of the last console write before the first frame
    #[arg(long, value_name = "NEEDLE=NAME")]
    pub console_milestone: Vec<MilestoneSpec>,
    /// Fail unless a milestone lands on this count. Repeatable
    #[arg(long, value_name = "NAME=N")]
    pub expect_milestone: Vec<NamedCount>,
    #[command(flatten)]
    pub capture: CaptureArgs,
    #[command(flatten)]
    pub observe: ObserveArgs,
}

#[derive(Args, Clone)]
pub struct BootCmd {
    #[command(flatten)]
    pub image: ImageArgs,
    #[command(flatten)]
    pub machine: MachineArgs,
    /// Instruction budget
    #[arg(long, short = 'n', value_name = "N", default_value_t = 10_000_000, value_parser = parse_count)]
    pub max_instructions: u64,
    /// How many recent program counters the report keeps
    #[arg(long, value_name = "N", default_value_t = RETIRED_PC_HISTORY)]
    pub retired_pcs: usize,
}

#[derive(Args, Clone)]
pub struct DisasmCmd {
    #[command(flatten)]
    pub image: ImageArgs,
    /// First address to decode
    #[arg(long, value_name = "ADDR", value_parser = parse_addr)]
    pub start: Option<u32>,
    /// How many instructions to decode
    #[arg(long, value_name = "N", default_value_t = 32, value_parser = parse_count)]
    pub count: u64,
}

#[derive(Args, Clone)]
pub struct HashCmd {
    #[command(subcommand)]
    pub kind: HashKind,
}

#[derive(Subcommand, Clone)]
pub enum HashKind {
    /// Hash a RAM image
    Ram {
        #[arg(long, value_name = "PATH")]
        file: PathBuf,
    },
    /// Hash a framebuffer followed by a palette
    Fb {
        #[arg(long, value_name = "PATH")]
        framebuffer: PathBuf,
        #[arg(long, value_name = "PATH")]
        palette: PathBuf,
    },
}

/// Anything that stops the command before the machine runs.
struct Failure {
    exit: Exit,
    message: String,
}

fn failed(message: impl Into<String>) -> Failure {
    Failure {
        exit: Exit::Failed,
        message: message.into(),
    }
}

fn usage(message: impl Into<String>) -> Failure {
    Failure {
        exit: Exit::Usage,
        message: message.into(),
    }
}

fn gate(message: impl Into<String>) -> Failure {
    Failure {
        exit: Exit::Gate,
        message: message.into(),
    }
}

/// A machine with its image loaded, and what is known about where it came
/// from.
struct Loaded {
    cpu: Cpu,
    rom_sha256: String,
    pinned: bool,
    manifest: Manifest,
}

fn load(
    image: &ImageArgs,
    machine: &MachineArgs,
    capture: &CaptureArgs,
) -> Result<Loaded, Failure> {
    let bytes = std::fs::read(&image.image)
        .map_err(|e| failed(format!("reading {}: {e}", image.image.display())))?;

    let rom_sha256 = sha256_hex(&bytes);
    let mut pinned = false;
    if let Some(path) = &image.pinned_hash {
        assert_pinned_hash(&bytes, path).map_err(|e| gate(e.to_string()))?;
        pinned = true;
    }

    let manifest_path = match (&image.manifest, image.no_manifest) {
        (Some(path), _) => Some(path.clone()),
        (None, true) => None,
        // A manifest beside the image is a convenience, so its absence is not
        // an error.
        (None, false) => {
            let beside = image.image.with_file_name("manifest.json");
            beside.exists().then_some(beside)
        }
    };
    let manifest = match &manifest_path {
        Some(path) => Manifest::read(path).map_err(|e| failed(e.to_string()))?,
        None => Manifest::default(),
    };

    let load_addr = image.load_addr.or(manifest.load_addr).unwrap_or(RAM_BASE);
    let parsed = match image.format {
        ImageFormat::Flat => Image::flat(bytes, load_addr),
        ImageFormat::Elf => Image::parse_elf(&bytes).map_err(|e| failed(e.to_string()))?,
        ImageFormat::Auto => {
            read_image(bytes, Some(load_addr)).map_err(|e| failed(e.to_string()))?
        }
    };

    // An explicit bound wins over the manifest, which wins over what the image
    // says about itself. Each is a default for the one below it rather than an
    // override of something the caller set.
    let text = match (
        image.text_start.or(manifest.text_start),
        image.text_end.or(manifest.text_end),
    ) {
        (Some(start), Some(end)) => Some((start, end)),
        _ => parsed.text_region(),
    };

    let map = MemoryMap::clickdoom().with_ram_size(machine.ram_size);
    let devices = match machine.devices {
        DeviceSet::Full => Devices::registers(machine.ipms),
        DeviceSet::None => Devices::bytes(map.mmio_size),
    };
    let mut cpu = Cpu::new(Memory::new(map, devices), parsed.entry);
    cpu.load(&parsed).map_err(|e| failed(e.to_string()))?;
    if let Some(entry) = image.entry {
        cpu.set_pc(entry);
    }
    cpu.set_text_region(text);

    // A resumed machine replaces everything the image just put there. The
    // image still loads first, because the region bounds and the map come
    // from it and the capture is checked against them.
    if let Some(path) = &capture.resume {
        let snapshot = Snapshot::read(path, &["ram", "framebuffer", "palette"])
            .map_err(|e| failed(e.to_string()))?;
        snapshot::restore(&mut cpu, &snapshot, path, capture.force_resume)
            .map_err(|e| failed(e.to_string()))?;
    }

    // Decoding the read-only region up front is what makes a long run cheap.
    // A machine with no declared region has nothing to cache.
    cpu.enable_decode_cache();

    Ok(Loaded {
        cpu,
        rom_sha256,
        pinned,
        manifest,
    })
}

/// How a run ended, and which condition ended it.
struct Ending {
    outcome: RunOutcome,
    condition: Option<Point>,
    halt: Option<Halt>,
    /// Snapshot points the run never reached.
    unreached: Vec<String>,
}

/// Reads a file of ADDR and NAME pairs, one per line, skipping comments.
///
/// The address column is hex without a prefix, matching every other address
/// column these reports write. A `0x` prefix is accepted too.
fn read_trap_file(path: &Path) -> Result<Vec<TrapSpec>, Failure> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| failed(format!("reading {}: {e}", path.display())))?;
    let mut specs = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (addr, name) = line
            .split_once(|c: char| c.is_whitespace())
            .ok_or_else(|| usage(format!("{}:{}: not ADDR NAME", path.display(), number + 1)))?;
        let addr = addr.trim();
        let digits = addr.strip_prefix("0x").unwrap_or(addr);
        specs.push(TrapSpec {
            addr: u32::from_str_radix(digits, 16).map_err(|_| {
                usage(format!(
                    "{}:{}: `{addr}` is not a hex address",
                    path.display(),
                    number + 1
                ))
            })?,
            name: name.trim().to_owned(),
        });
    }
    Ok(specs)
}

/// Sets up whatever the caller asked to record.
fn recorders(
    cpu: &Cpu,
    observe: &ObserveArgs,
    milestones: &[MilestoneSpec],
) -> Result<Recorders, Failure> {
    let text = cpu.memory.text_region();
    let mut specs = observe.trap_pc.clone();
    if let Some(path) = &observe.trap_pcs {
        specs.extend(read_trap_file(path)?);
    }
    Ok(Recorders {
        histogram: observe
            .pc_histogram
            .is_some()
            .then(|| PcHistogram::new(text)),
        traps: (!specs.is_empty())
            .then(|| Traps::new(&specs, observe.trap_regs.clone(), text, observe.trap_limit)),
        milestones: milestones
            .iter()
            .map(|spec| ConsoleMilestone::new(&spec.needle, &spec.name))
            .collect(),
        watch_writes: observe.watch_writes.iter().map(|r| r.region()).collect(),
        watching: false,
    })
}

/// Whether a run has reached a position.
fn reached(cpu: &Cpu, point: Point) -> bool {
    match point {
        Point::Icount(n) => cpu.icount() == n,
        Point::Frame(n) => cpu
            .memory
            .devices()
            .registers_ref()
            .is_some_and(|r| r.frame_commits.len() as u64 == n + 1),
        Point::Start => true,
        _ => false,
    }
}

/// The observer a command runs behind: it stops where the caller asked, feeds
/// the trace to a sink, and drives whatever recording was turned on.
struct Driver<'a, C: FnMut(Checkpoint)> {
    stops: Vec<Point>,
    fired: Option<Point>,
    watch_from: Point,
    watching: bool,
    watch_regions: Vec<Region>,
    histogram_at: Vec<Point>,
    taken: Vec<bool>,
    rec: &'a mut Recorders,
    sink: C,
}

impl<C: FnMut(Checkpoint)> Driver<'_, C> {
    /// Starts write watching once the run reaches its window, so the part
    /// before it costs nothing.
    fn maybe_start_watching(&mut self, cpu: &mut Cpu) {
        if self.watching || self.watch_regions.is_empty() || !reached(cpu, self.watch_from) {
            return;
        }
        cpu.memory.watch_writes(&self.watch_regions);
        self.watching = true;
    }

    fn maybe_snapshot(&mut self, cpu: &Cpu) {
        let Some(histogram) = &mut self.rec.histogram else {
            return;
        };
        for (index, point) in self.histogram_at.iter().enumerate() {
            if !self.taken[index] && reached(cpu, *point) {
                self.taken[index] = true;
                histogram.take_snapshot(point.to_string());
            }
        }
    }
}

impl<C: FnMut(Checkpoint)> Observer for Driver<'_, C> {
    fn before_step(&mut self, cpu: &mut Cpu) -> Step {
        self.maybe_start_watching(cpu);
        self.rec.before(cpu);
        Step::Continue
    }

    fn after_step(&mut self, cpu: &mut Cpu, retired_pc: u32, insn: crate::Instruction) -> Step {
        self.rec.after(retired_pc, insn);
        self.rec.after_console(cpu);
        self.maybe_start_watching(cpu);
        self.maybe_snapshot(cpu);
        for stop in &self.stops {
            if reached(cpu, *stop) {
                self.fired = Some(*stop);
                return Step::Stop;
            }
        }
        Step::Continue
    }

    fn checkpoint(&mut self, checkpoint: Checkpoint) {
        (self.sink)(checkpoint);
    }
}

/// Drives the machine to its first stop, emitting checkpoints.
fn drive<C>(
    cpu: &mut Cpu,
    config: TraceConfig,
    budget: &BudgetArgs,
    observe: &ObserveArgs,
    rec: &mut Recorders,
    on_checkpoint: C,
) -> Ending
where
    C: FnMut(Checkpoint),
{
    let conditions = budget.conditions();
    let histogram_at: Vec<Point> = observe.histogram_at.iter().map(|p| p.0).collect();
    let mut driver = Driver {
        stops: conditions.clone(),
        fired: None,
        watch_from: observe.watch_from.map_or(Point::Start, |w| w.0),
        watching: false,
        watch_regions: observe.watch_writes.iter().map(|r| r.region()).collect(),
        taken: vec![false; histogram_at.len()],
        histogram_at,
        rec,
        sink: on_checkpoint,
    };
    let stop = trace::run(cpu, config, budget.max_instructions, &mut driver);
    let fired = driver.fired;
    // A point named for a snapshot but never reached would leave a window
    // silently empty, so the run says so rather than reporting a difference
    // against nothing.
    let unreached: Vec<String> = driver
        .histogram_at
        .iter()
        .zip(&driver.taken)
        .filter(|(_, taken)| !**taken)
        .map(|(point, _)| point.to_string())
        .collect();
    if let Some(histogram) = &mut driver.rec.histogram {
        histogram.take_snapshot("end".to_owned());
    }

    match stop {
        Stop::Halted(halt) => Ending {
            outcome: RunOutcome::Halt,
            condition: conditions.contains(&Point::Halt).then_some(Point::Halt),
            halt: Some(halt),
            unreached,
        },
        Stop::Budget => Ending {
            outcome: RunOutcome::Budget,
            condition: conditions.contains(&Point::Budget).then_some(Point::Budget),
            halt: None,
            unreached,
        },
        Stop::Observer => Ending {
            outcome: RunOutcome::Stop,
            condition: fired,
            halt: None,
            unreached,
        },
    }
}

fn summarise(cpu: &Cpu, ending: &Ending, loaded_sha: &str, pinned: bool) -> RunReport {
    let commits: Vec<FrameCommitJson> = cpu
        .memory
        .devices()
        .registers_ref()
        .map(|r| {
            r.frame_commits
                .iter()
                .enumerate()
                .map(|(i, c)| FrameCommitJson::new(i as u64, *c))
                .collect()
        })
        .unwrap_or_default();
    let console = cpu
        .memory
        .devices()
        .registers_ref()
        .map_or(0, |r| r.console.len() as u64);

    RunReport {
        schema: report::RUN_REPORT_SCHEMA.to_owned(),
        outcome: ending.outcome,
        stop_condition: ending.condition.map(|c| c.to_string()),
        halted: ending.halt.is_some(),
        icount: cpu.icount(),
        pc: cpu.pc(),
        pc_hex: format!("{:08x}", cpu.pc()),
        halt: ending.halt.map(|h| HaltJson::new(h, cpu.icount())),
        reghash: format!("{:016x}", trace::reg_hash_of(cpu)),
        ramhash: format!("{:016x}", trace::ram_hash_of(cpu)),
        fbhash: format!("{:016x}", trace::fb_hash_of(cpu)),
        frame_commit_count: commits.len() as u64,
        first_frame_commit: commits.first().copied(),
        last_frame_commit: commits.last().copied(),
        console_bytes: console,
        rom_sha256: Some(loaded_sha.to_owned()),
        pinned,
    }
}

/// The exit code an ending earns, before any expectation is checked.
fn ending_exit(ending: &Ending) -> Exit {
    match ending.outcome {
        RunOutcome::Stop => Exit::Ok,
        RunOutcome::Halt if ending.condition.is_some() => Exit::Ok,
        RunOutcome::Halt => Exit::Halted,
        RunOutcome::Budget if ending.condition.is_some() => Exit::Ok,
        RunOutcome::Budget => Exit::Budget,
    }
}

fn check_expectations(report: &RunReport, expect: &ExpectArgs) -> Vec<String> {
    let mut failures = Vec::new();
    if let Some(want) = expect.expect_icount
        && report.icount != want
    {
        failures.push(format!(
            "expected to stop at icount {want}, stopped at {}",
            report.icount
        ));
    }
    for (want, got, name) in [
        (expect.expect_reghash, &report.reghash, "reghash"),
        (expect.expect_ramhash, &report.ramhash, "ramhash"),
        (expect.expect_fbhash, &report.fbhash, "fbhash"),
    ] {
        if let Some(want) = want {
            let want = format!("{want:016x}");
            if *got != want {
                failures.push(format!("expected {name} {want}, got {got}"));
            }
        }
    }
    failures
}

fn check_budget_reachable(budget: &BudgetArgs) -> Result<(), Failure> {
    for stop in &budget.stop_at {
        if let Point::Icount(n) = stop.0
            && n > budget.max_instructions
        {
            return Err(usage(format!(
                "--stop-at icount:{n} is past --max-instructions {}, so the run would \
                 stop short of it. Raise the budget.",
                budget.max_instructions
            )));
        }
    }
    Ok(())
}

/// Writes whatever a run was asked to record, and returns the checks that
/// did not hold.
fn write_records(
    cpu: &Cpu,
    ending: &Ending,
    observe: &ObserveArgs,
    rec: &Recorders,
) -> Result<Vec<String>, Failure> {
    let mut failures: Vec<String> = ending
        .unreached
        .iter()
        .map(|point| format!("the run never reached {point}, so its snapshot is missing"))
        .collect();

    if let (Some(path), Some(histogram)) = (&observe.pc_histogram, &rec.histogram) {
        let mut out = String::from("# refemu-pc-histogram 1\n");
        out.push_str("# columns\tsnapshot\tpc_hex\tcount\n");
        for snapshot in &histogram.snapshots {
            let counted: u64 = snapshot.rows.iter().map(|(_, count)| count).sum();
            out.push_str(&format!(
                "# snapshot\t{}\tretired={}\tdistinct={}\tcounted={counted}\n",
                snapshot.label,
                snapshot.retired,
                snapshot.rows.len()
            ));
        }
        for snapshot in &histogram.snapshots {
            for (pc, count) in &snapshot.rows {
                out.push_str(&format!("{}\t{pc:08x}\t{count}\n", snapshot.label));
            }
        }
        write_text(path, &out)?;
    }

    if let (Some(path), Some(traps)) = (&observe.trap_report, &rec.traps) {
        if traps.overflowed {
            return Err(failed(format!(
                "the trap log passed --trap-limit {} rows",
                traps.limit
            )));
        }
        let columns: Vec<String> = traps.regs.iter().map(|r| format!("x{r}")).collect();
        let mut out = String::from("# refemu-pc-traps 1\n");
        out.push_str(&format!(
            "# columns\ticount_before\tpc_hex\tname\t{}\n",
            columns.join("\t")
        ));
        for hit in &traps.hits {
            let regs: Vec<String> = hit.regs.iter().map(|v| v.to_string()).collect();
            out.push_str(&format!(
                "{}\t{:08x}\t{}\t{}\n",
                hit.icount_before,
                hit.pc,
                traps.names[hit.name_index],
                regs.join("\t")
            ));
        }
        write_text(path, &out)?;
    }

    if !observe.watch_writes.is_empty() {
        let map = *cpu.memory.map();
        let watch = cpu.memory.write_watch();
        let mut out = String::from("# refemu-write-coverage 1\n");
        out.push_str(&format!(
            "# window\tfrom={}\tto={}\n",
            observe
                .watch_from
                .map_or("start".to_owned(), |w| w.to_string()),
            ending
                .condition
                .map_or_else(|| "end".to_owned(), |c| c.to_string())
        ));
        out.push_str("# columns\tregion\twords_written\twords_total\tstores\n");
        for asked in &observe.watch_writes {
            let region = asked.region();
            let (written, total) = watch
                .and_then(|w| w.words_written(region, &map))
                .unwrap_or((0, 0));
            let stores = watch.map_or(0, |w| w.stores(region));
            out.push_str(&format!(
                "{}\t{written}\t{total}\t{stores}\n",
                region.as_str()
            ));
            if observe.require_coverage.contains(asked) && written != total {
                failures.push(format!(
                    "{} has {written} of {total} words written in the window",
                    region.as_str()
                ));
            }
        }
        if let Some(path) = &observe.write_coverage {
            write_text(path, &out)?;
        }
    }

    if let Some(path) = &observe.frame_log {
        let mut out = String::from("# refemu-frame-log 1\n");
        out.push_str("# columns\tindex\tframe_no\tcommit_icount\tretired_icount\n");
        if let Some(registers) = cpu.memory.devices().registers_ref() {
            for (index, commit) in registers.frame_commits.iter().enumerate() {
                out.push_str(&format!(
                    "{index}\t{}\t{}\t{}\n",
                    commit.frame_no,
                    commit.commit_icount,
                    commit.retired_icount()
                ));
            }
        }
        write_text(path, &out)?;
    }

    Ok(failures)
}

fn write_text(path: &Path, text: &str) -> Result<(), Failure> {
    if path == Path::new("-") {
        use std::io::Write as _;
        std::io::stdout()
            .write_all(text.as_bytes())
            .map_err(|e| failed(format!("writing to stdout: {e}")))
    } else {
        std::fs::write(path, text).map_err(|e| failed(format!("writing {}: {e}", path.display())))
    }
}

fn write_captures(
    cpu: &Cpu,
    loaded: &LoadedInfo<'_>,
    ending: &Ending,
    capture: &CaptureArgs,
) -> Result<(), Failure> {
    let from = Provenance {
        rom_sha256: Some(loaded.rom_sha256.to_owned()),
        pinned: loaded.pinned,
        rom_manifest: Some(loaded.manifest.clone()),
    };
    let stop = ending.condition.map(|c| c.to_string());
    if let Some(path) = &capture.dump_state {
        snapshot::machine_snapshot(cpu, from.clone(), stop.clone())
            .write(path)
            .map_err(|e| failed(e.to_string()))?;
    }
    if let Some(path) = &capture.dump_frame {
        snapshot::frame_snapshot(cpu, from, stop)
            .write(path)
            .map_err(|e| failed(e.to_string()))?;
    }
    Ok(())
}

/// What a capture records about where the image came from.
struct LoadedInfo<'a> {
    rom_sha256: &'a str,
    pinned: bool,
    manifest: &'a Manifest,
}

fn write_side_outputs(
    cpu: &Cpu,
    report: &RunReport,
    halt_report: Option<&PathBuf>,
    console_out: Option<&PathBuf>,
) -> Result<(), Failure> {
    if let Some(path) = halt_report {
        write_json(path, report).map_err(|e| failed(format!("writing {}: {e}", path.display())))?;
    }
    if let Some(path) = console_out {
        let bytes = cpu
            .memory
            .devices()
            .registers_ref()
            .map(|r| r.console.clone())
            .unwrap_or_default();
        std::fs::write(path, bytes)
            .map_err(|e| failed(format!("writing {}: {e}", path.display())))?;
    }
    Ok(())
}

fn report_ending(quiet: bool, cpu: &Cpu, ending: &Ending) {
    if quiet {
        return;
    }
    match (&ending.halt, ending.outcome) {
        (Some(halt), _) => {
            let mut detail = String::new();
            if let Some(insn) = halt.insn {
                detail.push_str(&format!(" insn=0x{insn:08x}"));
            }
            if let Some(addr) = halt.addr {
                detail.push_str(&format!(" addr=0x{addr:08x}"));
            }
            if let Some(code) = halt.exit_code {
                detail.push_str(&format!(" exit_code={code}"));
            }
            eprintln!(
                "# halted: {} at pc=0x{:08x} icount={}{detail}",
                halt.reason,
                halt.pc,
                cpu.icount()
            );
        }
        (None, RunOutcome::Budget) => {
            eprintln!(
                "# reached the instruction budget at icount={}",
                cpu.icount()
            )
        }
        (None, _) => eprintln!(
            "# stopped at {} (icount={})",
            ending
                .condition
                .map_or_else(|| "a stop".to_owned(), |c| c.to_string()),
            cpu.icount()
        ),
    }
}

fn cmd_run(cli: &Cli, cmd: &RunCmd) -> Result<Exit, Failure> {
    check_budget_reachable(&cmd.budget)?;
    let Loaded {
        mut cpu,
        rom_sha256,
        pinned,
        manifest,
    } = load(&cmd.image, &cmd.machine, &cmd.capture)?;
    let mut rec = recorders(&cpu, &cmd.observe, &[])?;
    let ending = drive(
        &mut cpu,
        TraceConfig::default(),
        &cmd.budget,
        &cmd.observe,
        &mut rec,
        |_| {},
    );
    let report = summarise(&cpu, &ending, &rom_sha256, pinned);
    let mut failures = write_records(&cpu, &ending, &cmd.observe, &rec)?;
    write_captures(
        &cpu,
        &LoadedInfo {
            rom_sha256: &rom_sha256,
            pinned,
            manifest: &manifest,
        },
        &ending,
        &cmd.capture,
    )?;
    write_side_outputs(
        &cpu,
        &report,
        cmd.halt_report.as_ref(),
        cmd.console_out.as_ref(),
    )?;
    report_ending(cli.quiet, &cpu, &ending);
    failures.extend(check_expectations(&report, &cmd.expect));
    if !failures.is_empty() {
        return Err(gate(failures.join("\n")));
    }
    Ok(ending_exit(&ending))
}

fn cmd_trace(cli: &Cli, cmd: &TraceCmd) -> Result<Exit, Failure> {
    check_budget_reachable(&cmd.budget)?;
    let config = TraceConfig {
        checkpoint_interval: cmd.checkpoint_interval,
        ram_hash_interval: cmd.ram_hash_interval,
    };
    config.validate().map_err(|e| usage(e.to_string()))?;

    let Loaded {
        mut cpu,
        rom_sha256,
        pinned,
        manifest,
    } = load(&cmd.image, &cmd.machine, &cmd.capture)?;

    // The trace's own name records which image it came from, so one generated
    // against an image that has since moved cannot be mistaken for current.
    let out_path = match (&cmd.out, &cmd.out_dir, &cmd.name) {
        (Some(path), _, _) => Some(path.clone()),
        (None, Some(dir), Some(name)) => {
            let file = clickdoom_spec::hashed_filename(name, &rom_sha256, ".tsv")
                .map_err(|e| failed(e.to_string()))?;
            std::fs::create_dir_all(dir)
                .map_err(|e| failed(format!("creating {}: {e}", dir.display())))?;
            Some(dir.join(file))
        }
        _ => None,
    };
    let meta_path = cmd
        .meta_out
        .clone()
        .or_else(|| match (&cmd.out_dir, &out_path) {
            (Some(_), Some(path)) => Some(path.with_extension("json")),
            _ => None,
        });

    let mut out: Box<dyn std::io::Write> = match &out_path {
        Some(path) if path != Path::new("-") => Box::new(std::io::BufWriter::new(
            std::fs::File::create(path)
                .map_err(|e| failed(format!("creating {}: {e}", path.display())))?,
        )),
        _ => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };

    // Hashed as it is written, so a twenty-megabyte trace is not read back to
    // find out what it hashes to, and one sent to stdout still has a hash.
    let mut digest = Sha256Stream::new();
    let mut trace_bytes = 0u64;
    let mut trace_lines = 0u64;
    let mut last_line: Option<String> = None;
    // purity-ok: reported in the metadata, never read by the machine.
    let started = std::time::Instant::now();

    let mut rec = recorders(&cpu, &cmd.observe, &cmd.console_milestone)?;
    let mut write_error = None;
    let ending = drive(
        &mut cpu,
        config,
        &cmd.budget,
        &cmd.observe,
        &mut rec,
        |checkpoint| {
            use std::io::Write as _;
            let line = format!("{checkpoint}\n");
            digest.update(line.as_bytes());
            trace_bytes += line.len() as u64;
            trace_lines += 1;
            last_line = Some(line[..line.len() - 1].to_owned());
            if write_error.is_none()
                && let Err(e) = out.write_all(line.as_bytes())
            {
                write_error = Some(e);
            }
        },
    );
    {
        use std::io::Write as _;
        out.flush()
            .map_err(|e| failed(format!("flushing the trace: {e}")))?;
    }
    if let Some(e) = write_error {
        return Err(failed(format!("writing the trace: {e}")));
    }

    let elapsed = started.elapsed();
    let report = summarise(&cpu, &ending, &rom_sha256, pinned);
    write_captures(
        &cpu,
        &LoadedInfo {
            rom_sha256: &rom_sha256,
            pinned,
            manifest: &manifest,
        },
        &ending,
        &cmd.capture,
    )?;
    write_side_outputs(
        &cpu,
        &report,
        cmd.halt_report.as_ref(),
        cmd.console_out.as_ref(),
    )?;
    report_ending(cli.quiet, &cpu, &ending);
    let mut failures = write_records(&cpu, &ending, &cmd.observe, &rec)?;
    failures.extend(check_expectations(&report, &cmd.expect));

    let milestones: Vec<Milestone> = rec
        .milestones
        .iter()
        .map(|m| Milestone {
            name: m.name.clone(),
            icount: m.found,
        })
        .collect();
    for expected in &cmd.expect_milestone {
        match milestones.iter().find(|m| m.name == expected.name) {
            Some(Milestone {
                icount: Some(at), ..
            }) if *at == expected.value => {}
            Some(Milestone {
                icount: Some(at), ..
            }) => failures.push(format!(
                "expected milestone {} at icount {}, got {at}",
                expected.name, expected.value
            )),
            _ => failures.push(format!(
                "milestone {} was never reached, and it is expected at icount {}",
                expected.name, expected.value
            )),
        }
    }

    if let Some(path) = &meta_path {
        let meta = TraceMeta {
            schema: report::TRACE_META_SCHEMA.to_owned(),
            spec_version: manifest.spec_version.clone(),
            refemu_version: env!("CARGO_PKG_VERSION").to_owned(),
            rom_sha256: rom_sha256.clone(),
            pinned,
            rom_manifest: Some(manifest.clone()),
            generated_by: generated_by(cmd),
            trace_file: out_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            trace_file_sha256: digest.finish(),
            trace_file_bytes: trace_bytes,
            trace_line_count: trace_lines,
            checkpoint_interval: config.checkpoint_interval,
            ram_hash_interval: config.ram_hash_interval,
            stop_condition: report.stop_condition.clone(),
            final_icount: cpu.icount(),
            final_checkpoint_line: last_line,
            final_state: FinalState {
                icount: cpu.icount(),
                pc: cpu.pc(),
                pc_hex: report.pc_hex.clone(),
                reghash: report.reghash.clone(),
                ramhash: report.ramhash.clone(),
                fbhash: report.fbhash.clone(),
            },
            halt: report.halt.clone(),
            frame_commit_count: report.frame_commit_count,
            first_frame_commit: report.first_frame_commit,
            last_frame_commit: report.last_frame_commit,
            milestones,
            timing: Timing {
                elapsed_seconds: elapsed.as_secs(),
                elapsed_millis: elapsed.subsec_millis(),
                instructions_per_second: (cpu.icount() as f64 / elapsed.as_secs_f64()) as u64,
            },
        };
        write_json(path, &meta).map_err(|e| failed(format!("writing {}: {e}", path.display())))?;
    }

    if !failures.is_empty() {
        return Err(gate(failures.join("\n")));
    }
    Ok(ending_exit(&ending))
}

/// The invocation that made a trace, rebuilt from the settings that shaped it
/// rather than copied from the command line.
///
/// A path pointing outside the repository resolves for nobody else, and the
/// content it named is already recorded here by hash, so what goes in the
/// record is the flags that decide the answer.
fn generated_by(cmd: &TraceCmd) -> String {
    let mut parts = vec!["refemu trace".to_owned()];
    parts.push(format!("-n {}", cmd.budget.max_instructions));
    for stop in &cmd.budget.stop_at {
        parts.push(format!("--stop-at {stop}"));
    }
    if cmd.checkpoint_interval != TraceConfig::default().checkpoint_interval {
        parts.push(format!("--checkpoint-interval {}", cmd.checkpoint_interval));
    }
    if cmd.ram_hash_interval != TraceConfig::default().ram_hash_interval {
        parts.push(format!("--ram-hash-interval {}", cmd.ram_hash_interval));
    }
    if let Some(name) = &cmd.name {
        parts.push(format!("--name {name}"));
    }
    if cmd.image.pinned_hash.is_some() {
        parts.push("--pinned-hash".to_owned());
    }
    for milestone in &cmd.console_milestone {
        parts.push(format!(
            "--console-milestone {}={}",
            milestone.needle, milestone.name
        ));
    }
    parts.join(" ")
}

fn cmd_boot(_cli: &Cli, cmd: &BootCmd) -> Result<Exit, Failure> {
    let Loaded { mut cpu, .. } = load(&cmd.image, &cmd.machine, &CaptureArgs::default())?;
    let report = crate::boot::boot(&mut cpu, cmd.max_instructions, cmd.retired_pcs);
    println!("{}", format_report(&report));
    Ok(match report.outcome {
        crate::boot::Outcome::FrameCommit => Exit::Ok,
        crate::boot::Outcome::Halt => {
            let clean = report.halt.is_some_and(|h| h.reason == HaltReason::Exit);
            if clean { Exit::Ok } else { Exit::Halted }
        }
        crate::boot::Outcome::BudgetExhausted => Exit::Budget,
    })
}

fn cmd_disasm(cmd: &DisasmCmd, machine: &MachineArgs) -> Result<Exit, Failure> {
    let Loaded { mut cpu, .. } = load(&cmd.image, machine, &CaptureArgs::default())?;
    let start = cmd.start.unwrap_or_else(|| cpu.pc());
    for index in 0..cmd.count {
        let addr = start.wrapping_add((index * 4) as u32);
        match cpu.memory.read(addr, 4, 0) {
            Ok(word) => println!("{addr:08x}\t{word:08x}\t{}", decode(word).render()),
            Err(_) => {
                eprintln!("# {addr:08x} is not readable");
                break;
            }
        }
    }
    Ok(Exit::Ok)
}

fn cmd_hash(cmd: &HashCmd) -> Result<Exit, Failure> {
    let digest = match &cmd.kind {
        HashKind::Ram { file } => {
            let bytes = std::fs::read(file)
                .map_err(|e| failed(format!("reading {}: {e}", file.display())))?;
            clickdoom_spec::ram_hash(&bytes)
        }
        HashKind::Fb {
            framebuffer,
            palette,
        } => {
            let fb = std::fs::read(framebuffer)
                .map_err(|e| failed(format!("reading {}: {e}", framebuffer.display())))?;
            let pal = std::fs::read(palette)
                .map_err(|e| failed(format!("reading {}: {e}", palette.display())))?;
            clickdoom_spec::fb_hash(&fb, &pal)
        }
    };
    println!("{digest:016x}");
    Ok(Exit::Ok)
}

/// Parses arguments and runs the command.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Run(cmd) => cmd_run(&cli, cmd),
        Command::Trace(cmd) => cmd_trace(&cli, cmd),
        Command::Boot(cmd) => cmd_boot(&cli, cmd),
        Command::Disasm(cmd) => {
            let machine = MachineArgs {
                ram_size: RAM_SIZE,
                ipms: IPMS_DEFAULT,
                devices: DeviceSet::None,
            };
            cmd_disasm(cmd, &machine)
        }
        Command::Hash(cmd) => cmd_hash(cmd),
    };
    match result {
        Ok(exit) => exit.into(),
        Err(failure) => {
            eprintln!("refemu: error: {}", failure.message);
            failure.exit.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_run_with_no_stop_accepts_its_budget_as_the_ending() {
        let budget = BudgetArgs {
            max_instructions: 10,
            stop_at: vec![],
        };
        assert_eq!(budget.conditions(), vec![Point::Budget]);
        let ending = Ending {
            outcome: RunOutcome::Budget,
            condition: Some(Point::Budget),
            halt: None,
            unreached: Vec::new(),
        };
        assert_eq!(ending_exit(&ending), Exit::Ok);
    }

    #[test]
    fn an_unasked_for_ending_has_its_own_code() {
        for (outcome, exit) in [
            (RunOutcome::Halt, Exit::Halted),
            (RunOutcome::Budget, Exit::Budget),
        ] {
            let ending = Ending {
                outcome,
                condition: None,
                halt: None,
                unreached: Vec::new(),
            };
            assert_eq!(ending_exit(&ending), exit);
        }
    }

    #[test]
    fn a_stop_past_the_budget_is_a_usage_error_rather_than_a_short_run() {
        let budget = BudgetArgs {
            max_instructions: 100,
            stop_at: vec![StopAt(Point::Icount(1000))],
        };
        let err = check_budget_reachable(&budget).unwrap_err();
        assert_eq!(err.exit, Exit::Usage);
        assert!(
            err.message.contains("past --max-instructions"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_expectation_that_does_not_hold_is_reported_by_name() {
        let mut report = RunReport {
            schema: String::new(),
            outcome: RunOutcome::Stop,
            stop_condition: None,
            halted: false,
            icount: 5,
            pc: 0,
            pc_hex: String::new(),
            halt: None,
            reghash: "0000000000000001".to_owned(),
            ramhash: "0000000000000002".to_owned(),
            fbhash: "0000000000000003".to_owned(),
            frame_commit_count: 0,
            first_frame_commit: None,
            last_frame_commit: None,
            console_bytes: 0,
            rom_sha256: None,
            pinned: false,
        };
        let expect = ExpectArgs {
            expect_icount: Some(6),
            expect_fbhash: Some(0xFF),
            ..ExpectArgs::default()
        };
        let failures = check_expectations(&report, &expect);
        assert_eq!(failures.len(), 2);
        assert!(failures[0].contains("icount 6"), "{:?}", failures);
        assert!(
            failures[1].contains("fbhash 00000000000000ff"),
            "{:?}",
            failures
        );

        report.icount = 6;
        report.fbhash = "00000000000000ff".to_owned();
        assert!(check_expectations(&report, &expect).is_empty());
    }
}
