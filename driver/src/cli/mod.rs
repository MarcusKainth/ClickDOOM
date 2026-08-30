//! The command line.
//!
//! Every subcommand shares one connection and [`ConnArgs`](crate::client::ConnArgs).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::bootstrap::{self, Seeded};
use crate::client::ConnArgs;
use crate::{decode, rom};

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

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(async {
        match &cli.command {
            Command::Ping(cmd) => cmd_ping(cmd).await,
            Command::LoadRom(cmd) => cmd_load_rom(cmd).await,
            Command::Bootstrap(cmd) => cmd_bootstrap(cmd).await,
            Command::Decode(cmd) => cmd_decode(cmd).await,
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
