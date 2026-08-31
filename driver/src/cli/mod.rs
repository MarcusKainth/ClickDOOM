//! The command line.
//!
//! Every subcommand shares one connection and [`ConnArgs`](crate::client::ConnArgs).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::bench;
use crate::bench::canonical;
use crate::bootstrap::{self, Seeded};
use crate::client::ConnArgs;
use crate::{decode, preflight, render, rom};

/// What the process reports.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Exit {
    /// The subcommand did what it was asked.
    Ok = 0,
    /// Could not do the job: a connection failed, a statement errored.
    Failed = 1,
    /// The command line is wrong. This is what clap already uses.
    Usage = 2,
    /// A pre-flight gate did not hold.
    Gate = 3,
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        ExitCode::from(exit as u8)
    }
}

#[derive(Parser)]
#[command(
    name = "clickdoom",
    version,
    about = "One persistent ClickHouse connection, shared by every subcommand."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Connect and report the server version
    Ping(PingCmd),
    /// Load a flat ROM binary into `ram`
    LoadRom(LoadRomCmd),
    /// Seed batch_id=0 with the reset state
    Bootstrap(BootstrapCmd),
    /// Rebuild `decoded` from `ram`'s current contents
    Decode(DecodeCmd),
    /// Read out the latest committed frame
    Render(RenderCmd),
    /// Check every gate before a long run
    Preflight(PreflightCmd),
    /// Run the resumable batch loop to a target icount
    Run(RunCmd),
    /// Run sqlcpu and refemu side by side, comparing every checkpoint
    Diff(DiffCmd),
    /// Real-ROM throughput benchmarks
    Bench(BenchCmd),
}

#[derive(Args)]
pub struct BenchCmd {
    #[command(subcommand)]
    pub mode: BenchMode,
}

#[derive(Subcommand)]
pub enum BenchMode {
    /// The canonical two-window, fold-alone-and-e2e throughput benchmark
    Canonical(CanonicalCmd),
    /// The canonical benchmark, head to head across ClickHouse images
    CompareVersions(CompareVersionsCmd),
    /// Render Markdown from a bench history file, no run needed
    Report(ReportCmd),
}

/// Default history file `bench canonical` appends to.
const CANONICAL_RESULTS_DEFAULT: &str = "rom/bench/canonical_throughput/results.jsonl";
/// Default history file `bench compare-versions` appends to.
const COMPARE_RESULTS_DEFAULT: &str = "rom/bench/version_compare/results.jsonl";

/// Batches an arm runs before it times anything. ClickHouse compiles an
/// expression DAG on its fourth execution (`min_count_to_compile_expression`
/// defaults to 3), and boot's memset loop holds the write log at the
/// high-water mark for the first three batches, so four untimed batches
/// clear both. The run checks the regime it actually got rather than
/// trusting this number.
const WARMUP_DEFAULT: u32 = 4;

/// How far `refemu` looks for the first frame. The pinned ROM announces one
/// at icount 15,393,136 (`CLICKDOOM_TARGET_ICOUNT`).
const FIRST_FRAME_MAX_INSTRUCTIONS_DEFAULT: u64 = 100_000_000;

#[derive(Args)]
pub struct CanonicalCmd {
    /// Flat ROM binary
    #[arg(long)]
    pub bin: PathBuf,
    /// Manifest naming the binary's size, sha256 and text region
    #[arg(long)]
    pub manifest: PathBuf,
    /// Docker image each arm starts its own container from
    #[arg(long)]
    pub image: String,
    /// Instructions per batch
    #[arg(long, default_value_t = clickdoom_executor::config::K_DEFAULT)]
    pub k: u32,
    /// Write-log high-water mark
    #[arg(long, default_value_t = clickdoom_executor::config::WRITE_LOG_HIGH_WATER_MARK_DEFAULT)]
    pub hwm: u32,
    /// Chained batches each arm runs before it times anything
    #[arg(long, default_value_t = WARMUP_DEFAULT)]
    pub warmup: u32,
    /// Timed chained batches per window per mode
    #[arg(long, default_value_t = 3)]
    pub batches: u32,
    /// How far refemu looks for the ROM's first frame
    #[arg(long, default_value_t = FIRST_FRAME_MAX_INSTRUCTIONS_DEFAULT)]
    pub first_frame_max_instructions: u64,
    /// Where the gameplay window's snapshot is cached
    #[arg(long, default_value = "/tmp/clickdoom-canonical-throughput")]
    pub snapshot_dir: PathBuf,
    /// The refemu binary, for generating the gameplay window's snapshot
    #[arg(long, default_value = "./target/release/refemu")]
    pub refemu_bin: PathBuf,
    /// History file to append this run's record to
    #[arg(long, default_value = CANONICAL_RESULTS_DEFAULT)]
    pub out: PathBuf,
    /// How quiet the machine was, recorded with the run
    #[arg(long)]
    pub note: Option<String>,
    /// Also print this run as Markdown (to PATH, or stdout if omitted)
    #[arg(long, num_args = 0..=1, default_missing_value = "-")]
    pub markdown: Option<PathBuf>,
}

#[derive(Args)]
pub struct CompareVersionsCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Flat ROM binary
    #[arg(long)]
    pub bin: PathBuf,
    /// Manifest naming the binary's size, sha256 and text region
    #[arg(long)]
    pub manifest: PathBuf,
    /// One arm per flag: NAME=<docker-image-ref>. Repeatable, at least 2
    #[arg(long = "arm", required = true, value_parser = crate::bench::compare::parse_arm)]
    pub arms: Vec<crate::bench::compare::Arm>,
    /// Instructions per batch
    #[arg(long, default_value_t = clickdoom_executor::config::K_DEFAULT)]
    pub k: u32,
    /// Write-log high-water mark
    #[arg(long, default_value_t = clickdoom_executor::config::WRITE_LOG_HIGH_WATER_MARK_DEFAULT)]
    pub hwm: u32,
    /// Rotations through every arm, to cancel warm-up-order bias
    #[arg(long, default_value_t = 3)]
    pub repeats: u32,
    /// Chained batches each arm runs before it times anything
    #[arg(long, default_value_t = WARMUP_DEFAULT)]
    pub warmup: u32,
    /// Timed chained batches per window per mode, per repeat
    #[arg(long, default_value_t = 3)]
    pub batches: u32,
    /// How far refemu looks for the ROM's first frame
    #[arg(long, default_value_t = FIRST_FRAME_MAX_INSTRUCTIONS_DEFAULT)]
    pub first_frame_max_instructions: u64,
    /// Where the gameplay window's snapshot is cached
    #[arg(long, default_value = "/tmp/clickdoom-canonical-throughput")]
    pub snapshot_dir: PathBuf,
    /// The refemu binary, for generating the gameplay window's snapshot
    #[arg(long, default_value = "./target/release/refemu")]
    pub refemu_bin: PathBuf,
    /// History file to append this comparison's record to
    #[arg(long, default_value = COMPARE_RESULTS_DEFAULT)]
    pub out: PathBuf,
    /// How quiet the machine was, recorded with the run
    #[arg(long)]
    pub note: Option<String>,
    /// Also print this run as Markdown (to PATH, or stdout if omitted)
    #[arg(long, num_args = 0..=1, default_missing_value = "-")]
    pub markdown: Option<PathBuf>,
}

#[derive(clap::ValueEnum, Clone)]
pub enum ReportKind {
    Canonical,
    CompareVersions,
}

#[derive(Args)]
pub struct ReportCmd {
    /// Which history file's record shape to render
    #[arg(long, value_enum)]
    pub kind: ReportKind,
    /// History file to read
    #[arg(long)]
    pub from: PathBuf,
    /// Which run to render: "latest" or a 0-based index
    #[arg(long, default_value = "latest")]
    pub run: String,
}

#[derive(Args)]
pub struct PingCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
}

#[derive(Args)]
pub struct LoadRomCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Flat ROM binary
    #[arg(long)]
    pub bin: PathBuf,
    /// Manifest naming the binary's size, sha256 and load address
    #[arg(long)]
    pub manifest: PathBuf,
    /// Word count of the RAM region `ram` is filled dense over
    #[arg(long, default_value_t = rom::RAM_WORDS_DEFAULT)]
    pub ram_words: u32,
}

#[derive(Args)]
pub struct BootstrapCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
}

#[derive(Args)]
pub struct DecodeCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Start of the read-only text region, in words
    #[arg(long)]
    pub text_start_word: u32,
    /// End of the read-only text region, in words
    #[arg(long)]
    pub text_end_word: u32,
}

#[derive(Args)]
pub struct RenderCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    #[command(subcommand)]
    pub mode: RenderMode,
}

#[derive(Subcommand)]
pub enum RenderMode {
    /// Insert a frames_out row from the latest committed frame
    Frame,
    /// Print the latest frame's fb_hash
    FbHash,
    /// Print the latest frame as ANSI half-block truecolor
    Ansi {
        #[arg(long, default_value_t = render::FB_WIDTH)]
        width: u32,
        #[arg(long, default_value_t = render::FB_HEIGHT)]
        height: u32,
    },
    /// Write the latest frame as a binary PPM (P6) image
    Ppm {
        /// Where to write the image
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = render::FB_WIDTH)]
        width: u32,
        #[arg(long, default_value_t = render::FB_HEIGHT)]
        height: u32,
    },
}

#[derive(Args)]
pub struct RunCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Flat ROM binary
    #[arg(long)]
    pub bin: PathBuf,
    /// Manifest naming the binary's size, sha256 and text region
    #[arg(long)]
    pub manifest: PathBuf,
    /// Instructions per batch
    #[arg(long)]
    pub k: u32,
    /// Write-log high-water mark
    #[arg(long)]
    pub hwm: u32,
    /// Reference checkpoint trace to diff against at every RAM_HASH_INTERVAL boundary
    #[arg(long)]
    pub trace: PathBuf,
    /// Instruction count to run to
    #[arg(long)]
    pub target_icount: u64,
    /// Stop cleanly once a committed frame's frame_no reaches this
    #[arg(long)]
    pub stop_at_frame: Option<u32>,
    /// Write every committed frame here as a binary PPM, named by frame_no.
    /// Created if absent
    #[arg(long)]
    pub frame_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct DiffCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Instruction count to compare both engines over
    pub n: u64,
    /// Flat ROM binary
    #[arg(long, default_value = "rom/build/doom-rv32im.bin")]
    pub bin: PathBuf,
    /// Manifest naming the binary's size, sha256 and text region
    #[arg(long, default_value = "rom/build/manifest.json")]
    pub manifest: PathBuf,
    /// Write-log high-water mark
    #[arg(long, default_value_t = 20_000)]
    pub hwm: u32,
    /// Ephemeral database name, dropped on exit
    #[arg(long, value_name = "NAME")]
    pub ephemeral_database: Option<String>,
    /// Leave the ephemeral database in place, for inspecting a caught divergence
    #[arg(long)]
    pub keep_db: bool,
    /// The reference emulator
    #[arg(long, default_value = "./target/release/refemu")]
    pub refemu_bin: PathBuf,
}

#[derive(Args)]
pub struct PreflightCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Flat ROM binary
    #[arg(long)]
    pub bin: PathBuf,
    /// Manifest naming the binary's size, sha256 and text region
    #[arg(long)]
    pub manifest: PathBuf,
    /// Instructions per batch the real run will use
    #[arg(long)]
    pub k: u32,
    /// Write-log high-water mark the real run will use
    #[arg(long)]
    pub hwm: u32,
}

/// Anything that stops the command before it does its job.
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

fn gate(message: impl Into<String>) -> Failure {
    Failure {
        exit: Exit::Gate,
        message: message.into(),
    }
}

async fn cmd_ping(cmd: &PingCmd) -> Result<Exit, Failure> {
    let db = cmd.conn.connect();
    let version: String = db
        .fetch_one("SELECT version()")
        .await
        .map_err(|err| failed(err.to_string()))?;
    println!("{version}");
    Ok(Exit::Ok)
}

async fn cmd_load_rom(cmd: &LoadRomCmd) -> Result<Exit, Failure> {
    let db = cmd.conn.connect();
    let loaded = rom::load(&db, &cmd.bin, &cmd.manifest, cmd.ram_words)
        .await
        .map_err(|err| failed(err.to_string()))?;
    eprintln!(
        "loaded {} words ({} bytes) at word_addr {}..{}",
        loaded.words,
        loaded.bytes,
        loaded.base_word,
        loaded.base_word + loaded.words - 1
    );
    eprintln!(
        "ram dense: {} rows, word_addr {}..{}",
        loaded.ram_words,
        loaded.base_word,
        loaded.base_word + loaded.ram_words - 1
    );
    Ok(Exit::Ok)
}

async fn cmd_bootstrap(cmd: &BootstrapCmd) -> Result<Exit, Failure> {
    let db = cmd.conn.connect();
    match bootstrap::seed(&db, &bootstrap::RESET_REGS)
        .await
        .map_err(|err| failed(err.to_string()))?
    {
        Seeded::Fresh { registers } => eprintln!(
            "seeded batch_commit batch_id=0: pc={:#010x}, {registers} registers, icount=0",
            clickdoom_spec::RAM_BASE
        ),
        Seeded::AlreadySeeded => {
            eprintln!("batch_commit already has a batch_id=0 row -- not seeding again")
        }
    }
    Ok(Exit::Ok)
}

async fn cmd_decode(cmd: &DecodeCmd) -> Result<Exit, Failure> {
    let db = cmd.conn.connect();
    decode::decode(
        &db,
        &cmd.conn.database,
        cmd.text_start_word,
        cmd.text_end_word,
    )
    .await
    .map_err(|err| failed(err.to_string()))?;
    eprintln!("decoded {}..{}", cmd.text_start_word, cmd.text_end_word);
    Ok(Exit::Ok)
}

async fn cmd_render(cmd: &RenderCmd) -> Result<Exit, Failure> {
    let db = cmd.conn.connect();
    match &cmd.mode {
        RenderMode::Frame => {
            db.run(&render::frame_readout_sql(&cmd.conn.database))
                .await
                .map_err(|err| failed(err.to_string()))?;
        }
        RenderMode::FbHash => {
            let hash: String = db
                .fetch_one(&render::frame_readout_fb_hash_sql(&cmd.conn.database))
                .await
                .map_err(|err| failed(err.to_string()))?;
            println!("{hash}");
        }
        RenderMode::Ansi { width, height } => {
            let ansi: String = db
                .fetch_one(&render::ansi_render_sql(
                    &cmd.conn.database,
                    *width,
                    *height,
                ))
                .await
                .map_err(|err| failed(err.to_string()))?;
            println!("{ansi}");
        }
        RenderMode::Ppm { out, width, height } => {
            let ppm: bytes::Bytes = db
                .fetch_one(&render::ppm_render_sql(&cmd.conn.database, *width, *height))
                .await
                .map_err(|err| failed(err.to_string()))?;
            std::fs::write(out, &ppm).map_err(|err| failed(err.to_string()))?;
        }
    }
    Ok(Exit::Ok)
}

async fn cmd_run(cmd: &RunCmd) -> Result<Exit, Failure> {
    let args = crate::run::Args {
        bin: &cmd.bin,
        manifest_path: &cmd.manifest,
        k: cmd.k,
        hwm: cmd.hwm,
        trace_path: &cmd.trace,
        target_icount: cmd.target_icount,
        stop_at_frame: cmd.stop_at_frame,
        frame_dir: cmd.frame_dir.as_deref(),
    };
    let outcome = crate::run::run(&cmd.conn, &args)
        .await
        .map_err(|err| failed(err.to_string()))?;
    eprintln!("final_batch_id\t{}", outcome.final_batch_id);
    eprintln!("final_icount\t{}", outcome.final_icount);
    eprintln!("frames_observed\t{}", outcome.frames_observed);
    match outcome.stop {
        crate::run::Stop::Interrupted => eprintln!("stop\tinterrupted"),
        crate::run::Stop::ReachedTarget => eprintln!("stop\treached_target"),
        crate::run::Stop::HaltedAtOrPastTarget { reason } => {
            eprintln!("stop\thalted\treason\t{reason}")
        }
    }
    Ok(Exit::Ok)
}

async fn cmd_diff(cmd: &DiffCmd) -> Result<Exit, Failure> {
    let database = cmd
        .ephemeral_database
        .clone()
        .unwrap_or_else(|| format!("clickdoom_diff_{}", std::process::id()));
    let args = crate::diff::Args {
        bin: &cmd.bin,
        manifest_path: &cmd.manifest,
        hwm: cmd.hwm,
        database,
        keep_db: cmd.keep_db,
        refemu_bin: cmd.refemu_bin.clone(),
        target_icount: cmd.n,
    };
    let outcome = crate::diff::run(&cmd.conn, &args).await.map_err(|err| {
        use crate::diff::DiffError;
        match err {
            DiffError::RomHash(..)
            | DiffError::CheckpointMismatch { .. }
            | DiffError::SqlcpuOutranRefemuHalt { .. }
            | DiffError::SqlcpuHaltedAlone { .. }
            | DiffError::RefemuHaltedAlone { .. }
            | DiffError::HaltShapeMismatch { .. }
            | DiffError::OracleTraceShortfall { .. }
            | DiffError::CountShortfall { .. } => gate(err.to_string()),
            DiffError::Read { .. }
            | DiffError::HaltReportParse { .. }
            | DiffError::Manifest(_)
            | DiffError::Spawn(..)
            | DiffError::RefemuFailed(..)
            | DiffError::NoTraceLine(..)
            | DiffError::Db(_)
            | DiffError::Bootstrap(_)
            | DiffError::Provision(_) => failed(err.to_string()),
        }
    })?;
    eprintln!(
        "diff: no divergence found -- {} register checkpoints compared through icount={} ({} of them also memory+framebuffer checkpoints)",
        outcome.checkpoints_compared, outcome.final_icount, outcome.ram_hash_checkpoints_compared
    );
    eprintln!("rom_sha256\t{}", outcome.rom_sha256);
    eprintln!("clickhouse_version\t{}", outcome.clickhouse_version);
    eprintln!("requested_instructions\t{}", outcome.requested_instructions);
    eprintln!("final_icount\t{}", outcome.final_icount);
    eprintln!("batches_run\t{}", outcome.batches_run);
    eprintln!("checkpoints_compared\t{}", outcome.checkpoints_compared);
    eprintln!("checkpoints_expected\t{}", outcome.checkpoints_expected);
    eprintln!(
        "ram_hash_checkpoints_compared\t{}",
        outcome.ram_hash_checkpoints_compared
    );
    eprintln!(
        "ram_hash_checkpoints_expected\t{}",
        outcome.ram_hash_checkpoints_expected
    );
    eprintln!("sqlcpu_halted\t{}", outcome.sqlcpu_halted as u8);
    Ok(Exit::Ok)
}

async fn cmd_preflight(cmd: &PreflightCmd) -> Result<Exit, Failure> {
    let db = cmd.conn.connect();
    let provenance = preflight::check(&db, &cmd.conn, &cmd.bin, &cmd.manifest, cmd.k, cmd.hwm)
        .await
        .map_err(|err| match err {
            preflight::GateError::Db(_)
            | preflight::GateError::Read { .. }
            | preflight::GateError::Manifest(_) => failed(err.to_string()),
            preflight::GateError::Decoded(_)
            | preflight::GateError::Ram(_)
            | preflight::GateError::RomHash(_)
            | preflight::GateError::Smoke(_)
            | preflight::GateError::Schema(_) => gate(err.to_string()),
        })?;
    eprintln!("all 5 pre-flight gates passed");
    eprintln!("rom_sha256\t{}", provenance.rom_sha256);
    eprintln!("decoded_rows\t{}", provenance.decoded_rows);
    eprintln!("K\t{}", provenance.k);
    eprintln!("HWM\t{}", provenance.hwm);
    eprintln!("database\t{}", provenance.database);
    Ok(Exit::Ok)
}

async fn cmd_bench(cmd: &BenchCmd) -> Result<Exit, Failure> {
    match &cmd.mode {
        BenchMode::Canonical(cmd) => cmd_bench_canonical(cmd).await,
        BenchMode::CompareVersions(cmd) => cmd_bench_compare_versions(cmd).await,
        BenchMode::Report(cmd) => cmd_bench_report(cmd),
    }
}

fn write_markdown(target: &std::path::Path, text: &str) -> Result<(), Failure> {
    if target == std::path::Path::new("-") {
        println!("{text}");
    } else {
        std::fs::write(target, text).map_err(|e| failed(format!("{}: {e}", target.display())))?;
    }
    Ok(())
}

async fn cmd_bench_canonical(cmd: &CanonicalCmd) -> Result<Exit, Failure> {
    let args = canonical::Args {
        bin: &cmd.bin,
        manifest_path: &cmd.manifest,
        image: &cmd.image,
        k: cmd.k,
        hwm: cmd.hwm,
        warmup: cmd.warmup,
        batches: cmd.batches,
        first_frame_max_instructions: cmd.first_frame_max_instructions,
        windows: canonical::Windows::default(),
        snapshot_dir: cmd.snapshot_dir.clone(),
        refemu_bin: cmd.refemu_bin.clone(),
    };
    let report = canonical::run(&args)
        .await
        .map_err(|err| failed(err.to_string()))?;
    eprintln!("rom_sha256\t{}", report.rom_sha256);
    eprintln!("decoded_rows\t{}", report.decoded_rows);
    eprintln!("image\t{}", report.image);
    eprintln!("clickhouse_version\t{}", report.clickhouse_version);
    eprintln!("K\t{}", report.k);
    eprintln!("HWM\t{}", report.hwm);
    eprintln!("warmup_batches_per_arm\t{}", report.warmup);
    eprintln!("timed_batches_per_arm\t{}", report.batches);
    eprintln!(
        "instructions_to_first_frame\t{}",
        report.first_frame.instructions
    );
    eprintln!("first_frame_no\t{}", report.first_frame.frame_no);
    eprintln!("git_sha\t{}", report.git_sha);
    println!(
        "window\tmode\tk\thwm\tretired\tinstr_per_sec\tseconds_to_first_frame\twrite_log_len\tcompile_function\tcompile_micros"
    );
    for window in &report.windows {
        for (mode, arm) in [("fold", &window.fold), ("e2e", &window.e2e)] {
            let timed = || arm.batches.iter().filter(|b| b.timed);
            let write_log_len = timed().map(|b| b.write_log_len).max().unwrap_or(0);
            let compile_function: u64 = timed().map(|b| b.regime.compile_function).sum();
            let compile_micros: u64 = timed().map(|b| b.regime.compile_micros).sum();
            println!(
                "{}\t{mode}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{write_log_len}\t{compile_function}\t{compile_micros}",
                window.label,
                window.k,
                window.hwm,
                arm.retired,
                arm.instr_per_sec(),
                arm.seconds_to_first_frame(&report.first_frame)
            );
        }
    }

    let mut record = bench::report::CanonicalRecord::from(&report);
    record.note = cmd.note.clone();
    bench::report::append_canonical(&cmd.out, &record).map_err(|err| failed(err.to_string()))?;
    if let Some(target) = &cmd.markdown {
        write_markdown(target, &bench::report::render_canonical(&record))?;
    }
    Ok(Exit::Ok)
}

async fn cmd_bench_compare_versions(cmd: &CompareVersionsCmd) -> Result<Exit, Failure> {
    if cmd.arms.len() < 2 {
        return Err(failed("--arm needs at least 2 entries to compare anything"));
    }
    let args = bench::compare::Args {
        bin: &cmd.bin,
        manifest_path: &cmd.manifest,
        k: cmd.k,
        hwm: cmd.hwm,
        repeats: cmd.repeats,
        warmup: cmd.warmup,
        batches: cmd.batches,
        first_frame_max_instructions: cmd.first_frame_max_instructions,
        windows: canonical::Windows::default(),
        snapshot_dir: cmd.snapshot_dir.clone(),
        refemu_bin: cmd.refemu_bin.clone(),
        arms: cmd.arms.clone(),
        note: cmd.note.clone(),
    };
    let record = bench::compare::run(&args)
        .await
        .map_err(|err| failed(err.to_string()))?;
    bench::report::append_compare(&cmd.out, &record).map_err(|err| failed(err.to_string()))?;
    let markdown = bench::report::render_compare(&record);
    if let Some(target) = &cmd.markdown {
        write_markdown(target, &markdown)?;
    } else {
        println!("{markdown}");
    }
    Ok(Exit::Ok)
}

fn cmd_bench_report(cmd: &ReportCmd) -> Result<Exit, Failure> {
    let which = if cmd.run == "latest" {
        bench::report::Selector::Latest
    } else {
        let i: usize = cmd.run.parse().map_err(|_| {
            failed(format!(
                "--run must be \"latest\" or a 0-based index, got {:?}",
                cmd.run
            ))
        })?;
        bench::report::Selector::Index(i)
    };
    let markdown = match cmd.kind {
        ReportKind::Canonical => {
            let record = bench::report::select_canonical(&cmd.from, which)
                .map_err(|e| failed(e.to_string()))?;
            bench::report::render_canonical(&record)
        }
        ReportKind::CompareVersions => {
            let record = bench::report::select_compare(&cmd.from, which)
                .map_err(|e| failed(e.to_string()))?;
            bench::report::render_compare(&record)
        }
    };
    println!("{markdown}");
    Ok(Exit::Ok)
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(async {
        match &cli.command {
            Command::Ping(cmd) => cmd_ping(cmd).await,
            Command::LoadRom(cmd) => cmd_load_rom(cmd).await,
            Command::Bootstrap(cmd) => cmd_bootstrap(cmd).await,
            Command::Decode(cmd) => cmd_decode(cmd).await,
            Command::Render(cmd) => cmd_render(cmd).await,
            Command::Preflight(cmd) => cmd_preflight(cmd).await,
            Command::Run(cmd) => cmd_run(cmd).await,
            Command::Diff(cmd) => cmd_diff(cmd).await,
            Command::Bench(cmd) => cmd_bench(cmd).await,
        }
    });
    match result {
        Ok(exit) => exit.into(),
        Err(failure) => {
            eprintln!("clickdoom: error: {}", failure.message);
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
}
