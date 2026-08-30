//! The command line.
//!
//! Every subcommand shares one connection and [`ConnArgs`](crate::client::ConnArgs).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

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
}

#[derive(Args)]
pub struct CanonicalCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Flat ROM binary
    #[arg(long)]
    pub bin: PathBuf,
    /// Manifest naming the binary's size, sha256 and text region
    #[arg(long)]
    pub manifest: PathBuf,
    /// Instructions per batch
    #[arg(long, default_value_t = clickdoom_executor::config::K_DEFAULT)]
    pub k: u32,
    /// Write-log high-water mark
    #[arg(long, default_value_t = clickdoom_executor::config::WRITE_LOG_HIGH_WATER_MARK_DEFAULT)]
    pub hwm: u32,
    /// Chained batches per window per mode
    #[arg(long, default_value_t = 3)]
    pub batches: u32,
    /// Where the gameplay window's snapshot is cached
    #[arg(long, default_value = "/tmp/clickdoom-canonical-throughput")]
    pub snapshot_dir: PathBuf,
    /// The refemu binary, for generating the gameplay window's snapshot
    #[arg(long, default_value = "./target/release/refemu")]
    pub refemu_bin: PathBuf,
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
    }
}

async fn cmd_bench_canonical(cmd: &CanonicalCmd) -> Result<Exit, Failure> {
    let args = canonical::Args {
        bin: &cmd.bin,
        manifest_path: &cmd.manifest,
        k: cmd.k,
        hwm: cmd.hwm,
        batches: cmd.batches,
        windows: canonical::Windows::default(),
        snapshot_dir: cmd.snapshot_dir.clone(),
        refemu_bin: cmd.refemu_bin.clone(),
    };
    let report = canonical::run(&cmd.conn, &args)
        .await
        .map_err(|err| failed(err.to_string()))?;
    eprintln!("rom_sha256\t{}", report.rom_sha256);
    eprintln!("decoded_rows\t{}", report.decoded_rows);
    eprintln!("K\t{}", report.k);
    eprintln!("HWM\t{}", report.hwm);
    eprintln!("batches_per_mode\t{}", report.batches);
    eprintln!("git_sha\t{}", report.git_sha);
    println!("window\tmode\tk\thwm\tretired\tinstr_per_sec");
    for window in &report.windows {
        println!(
            "{}\tfold\t{}\t{}\t{}\t{:.1}",
            window.label,
            window.k,
            window.hwm,
            window.fold.retired,
            window.fold.instr_per_sec()
        );
        println!(
            "{}\te2e\t{}\t{}\t{}\t{:.1}",
            window.label,
            window.k,
            window.hwm,
            window.e2e.retired,
            window.e2e.instr_per_sec()
        );
    }
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
