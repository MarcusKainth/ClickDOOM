//! The command line.
//!
//! Every subcommand shares one connection and [`ConnArgs`](crate::client::ConnArgs).

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::client::ConnArgs;

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
}

#[derive(Args)]
pub struct PingCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
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

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(async {
        match &cli.command {
            Command::Ping(cmd) => cmd_ping(cmd).await,
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
