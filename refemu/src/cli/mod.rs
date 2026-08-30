//! The command line.
//!
//! A subcommand owns a contract: the bytes it writes and where it writes them.
//! A flag owns an observation, which is one more thing to record about the
//! same pass over the same program. Profiling is a set of flags rather than a
//! subcommand, because it needs every option `run` already has and would
//! otherwise restate them and then drift.

pub mod point;
pub mod report;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clickdoom_spec::{
    Checkpoint, HaltReason, IPMS_DEFAULT, Manifest, MemoryMap, RAM_BASE, RAM_SIZE, TraceConfig,
    assert_pinned_hash, sha256_hex,
};

use crate::boot::{RETIRED_PC_HISTORY, format_report};
use crate::decode::decode;
use crate::exec::{Cpu, Halt};
use crate::image::{Image, read_image};
use crate::memory::Memory;
use crate::mmio::Devices;
use crate::trace::{self, Step, Stop};
use point::{Point, StopAt, parse_addr, parse_count, parse_hash64};
use report::{FrameCommitJson, HaltJson, RunOutcome, RunReport, write_json};

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
}

fn load(image: &ImageArgs, machine: &MachineArgs) -> Result<Loaded, Failure> {
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
    // Decoding the read-only region up front is what makes a long run cheap.
    // A machine with no declared region has nothing to cache.
    cpu.enable_decode_cache();

    Ok(Loaded {
        cpu,
        rom_sha256,
        pinned,
    })
}

/// How a run ended, and which condition ended it.
struct Ending {
    outcome: RunOutcome,
    condition: Option<Point>,
    halt: Option<Halt>,
}

/// Drives the machine to its first stop, emitting checkpoints.
fn drive<C>(cpu: &mut Cpu, config: TraceConfig, budget: &BudgetArgs, on_checkpoint: C) -> Ending
where
    C: FnMut(Checkpoint),
{
    let conditions = budget.conditions();
    let mut fired: Option<Point> = None;
    let stop = trace::run(
        cpu,
        config,
        budget.max_instructions,
        |cpu| {
            for condition in &conditions {
                let hit = match condition {
                    Point::Icount(n) => cpu.icount() == *n,
                    Point::Frame(n) => cpu
                        .memory
                        .devices()
                        .registers_ref()
                        .is_some_and(|r| r.frame_commits.len() as u64 == *n + 1),
                    _ => false,
                };
                if hit {
                    fired = Some(*condition);
                    return Step::Stop;
                }
            }
            Step::Continue
        },
        on_checkpoint,
    );

    match stop {
        Stop::Halted(halt) => Ending {
            outcome: RunOutcome::Halt,
            condition: conditions.contains(&Point::Halt).then_some(Point::Halt),
            halt: Some(halt),
        },
        Stop::Budget => Ending {
            outcome: RunOutcome::Budget,
            condition: conditions.contains(&Point::Budget).then_some(Point::Budget),
            halt: None,
        },
        Stop::Observer => Ending {
            outcome: RunOutcome::Stop,
            condition: fired,
            halt: None,
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
    } = load(&cmd.image, &cmd.machine)?;
    let ending = drive(&mut cpu, TraceConfig::default(), &cmd.budget, |_| {});
    let report = summarise(&cpu, &ending, &rom_sha256, pinned);
    write_side_outputs(
        &cpu,
        &report,
        cmd.halt_report.as_ref(),
        cmd.console_out.as_ref(),
    )?;
    report_ending(cli.quiet, &cpu, &ending);
    let failures = check_expectations(&report, &cmd.expect);
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
    } = load(&cmd.image, &cmd.machine)?;

    let mut out: Box<dyn std::io::Write> = match &cmd.out {
        Some(path) if path != Path::new("-") => Box::new(std::io::BufWriter::new(
            std::fs::File::create(path)
                .map_err(|e| failed(format!("creating {}: {e}", path.display())))?,
        )),
        _ => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };

    let mut write_error = None;
    let ending = drive(&mut cpu, config, &cmd.budget, |checkpoint| {
        use std::io::Write as _;
        if write_error.is_none()
            && let Err(e) = writeln!(out, "{checkpoint}")
        {
            write_error = Some(e);
        }
    });
    {
        use std::io::Write as _;
        out.flush()
            .map_err(|e| failed(format!("flushing the trace: {e}")))?;
    }
    if let Some(e) = write_error {
        return Err(failed(format!("writing the trace: {e}")));
    }

    let report = summarise(&cpu, &ending, &rom_sha256, pinned);
    write_side_outputs(
        &cpu,
        &report,
        cmd.halt_report.as_ref(),
        cmd.console_out.as_ref(),
    )?;
    report_ending(cli.quiet, &cpu, &ending);
    let failures = check_expectations(&report, &cmd.expect);
    if !failures.is_empty() {
        return Err(gate(failures.join("\n")));
    }
    Ok(ending_exit(&ending))
}

fn cmd_boot(_cli: &Cli, cmd: &BootCmd) -> Result<Exit, Failure> {
    let Loaded { mut cpu, .. } = load(&cmd.image, &cmd.machine)?;
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
    let Loaded { mut cpu, .. } = load(&cmd.image, machine)?;
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
