//! The command line, in two namespaces.
//!
//! [`emulation`] runs the RV32IM CPU that lives in SQL against the DOOM ROM.
//! [`native`] runs DOOM's own simulation and renderer as SQL. Every
//! subcommand under either shares one connection and
//! [`ConnArgs`](crate::client::ConnArgs).

pub mod emulation;
pub mod native;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

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

// The variants differ in size by the whole emulation argument tree, and
// clap's derive takes a subcommand's payload inline rather than boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub enum Command {
    /// The RV32IM CPU in SQL, executing the DOOM ROM
    Emulation(emulation::EmulationCmd),
    // Described by `NativeCmd`'s own `about` and `long_about`. A doc comment
    // here would replace both with its first line.
    Native(native::NativeCmd),
}

/// Anything that stops a subcommand before it does its job.
pub(crate) struct Failure {
    pub exit: Exit,
    pub message: String,
}

pub(crate) fn failed(message: impl Into<String>) -> Failure {
    Failure {
        exit: Exit::Failed,
        message: message.into(),
    }
}

pub(crate) fn gate(message: impl Into<String>) -> Failure {
    Failure {
        exit: Exit::Gate,
        message: message.into(),
    }
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(async {
        match &cli.command {
            Command::Emulation(cmd) => emulation::run(cmd).await,
            Command::Native(cmd) => native::run(cmd).await,
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

    /// One parseable argument list per `emulation` subcommand. A subcommand
    /// missing from here is covered by no test below.
    const EMULATION_LINES: &[&[&str]] = &[
        &["ping"],
        &["load-rom", "--bin", "d.bin", "--manifest", "m.json"],
        &["bootstrap"],
        &["decode", "--text-start-word", "0", "--text-end-word", "1"],
        &["render", "frame"],
        &["render", "fb-hash"],
        &["render", "ansi"],
        &["render", "ppm", "--out", "f.ppm"],
        &[
            "preflight",
            "--bin",
            "d.bin",
            "--manifest",
            "m.json",
            "--k",
            "1",
            "--hwm",
            "1",
        ],
        &[
            "run",
            "--bin",
            "d.bin",
            "--manifest",
            "m.json",
            "--k",
            "1",
            "--hwm",
            "1",
            "--trace",
            "t.tsv",
            "--target-icount",
            "1",
        ],
        &["diff", "100000"],
        &[
            "bench",
            "canonical",
            "--bin",
            "d.bin",
            "--manifest",
            "m.json",
            "--image",
            "clickhouse/clickhouse-server:26.7.5.10",
        ],
        &[
            "bench",
            "compare-versions",
            "--bin",
            "d.bin",
            "--manifest",
            "m.json",
            "--arm",
            "a=image-a",
            "--arm",
            "b=image-b",
        ],
        &[
            "bench",
            "report",
            "--kind",
            "canonical",
            "--from",
            "r.jsonl",
        ],
    ];

    fn argv<'a>(prefix: &[&'a str], rest: &[&'a str]) -> Vec<&'a str> {
        let mut all = prefix.to_vec();
        all.extend_from_slice(rest);
        all
    }

    #[test]
    fn the_command_line_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn every_emulation_subcommand_parses_under_the_namespace() {
        for line in EMULATION_LINES {
            let args = argv(&["clickdoom", "emulation"], line);
            if let Err(err) = Cli::try_parse_from(&args) {
                panic!("{args:?} did not parse: {err}");
            }
        }
    }

    /// The old top-level spellings are gone rather than aliased, so a script
    /// still using one fails loudly instead of running something else.
    #[test]
    fn no_emulation_subcommand_answers_at_the_top_level() {
        for line in EMULATION_LINES {
            let args = argv(&["clickdoom"], line);
            assert!(
                Cli::try_parse_from(&args).is_err(),
                "{args:?} still parses at the top level"
            );
        }
    }

    #[test]
    fn emulation_needs_a_subcommand() {
        assert!(Cli::try_parse_from(["clickdoom", "emulation"]).is_err());
    }

    /// One parseable argument list per `native` subcommand, same roster
    /// rule as `EMULATION_LINES`.
    const NATIVE_LINES: &[&[&str]] = &[
        &["load"],
        &["load", "--wad", "w.wad", "--map", "E1M1", "--demo", "DEMO1"],
        &["load", "--probe", "p.tsv"],
        &["session-check"],
        &["session-check", "--rows", "10"],
    ];

    #[test]
    fn every_native_subcommand_parses_under_the_namespace() {
        for line in NATIVE_LINES {
            let args = argv(&["clickdoom", "native"], line);
            if let Err(err) = Cli::try_parse_from(&args) {
                panic!("{args:?} did not parse: {err}");
            }
        }
    }

    #[test]
    fn native_needs_a_subcommand() {
        assert!(Cli::try_parse_from(["clickdoom", "native"]).is_err());
        assert!(Cli::try_parse_from(["clickdoom", "native", "nope"]).is_err());
    }

    #[test]
    fn no_native_subcommand_answers_at_the_top_level() {
        for line in NATIVE_LINES {
            let args = argv(&["clickdoom"], line);
            assert!(
                Cli::try_parse_from(&args).is_err(),
                "{args:?} still parses at the top level"
            );
        }
    }

    #[test]
    fn session_check_takes_the_shared_connection_flags() {
        let cli = Cli::try_parse_from([
            "clickdoom",
            "native",
            "session-check",
            "--host",
            "elsewhere",
            "--database",
            "probe",
            "--max-p50-ms",
            "2.5",
        ])
        .expect("session-check takes the connection flags");
        let Command::Native(cmd) = &cli.command else {
            panic!("parsed something other than native");
        };
        let native::Command::SessionCheck(check) = &cmd.command else {
            panic!("parsed something other than session-check");
        };
        assert_eq!(check.conn.host, "elsewhere");
        assert_eq!(check.conn.database, "probe");
        assert_eq!(check.max_p50_ms, 2.5);
    }
}
