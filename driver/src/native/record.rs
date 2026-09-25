//! What one differential run found, as one JSON line.
//!
//! `clickdoom native diff --record` appends a [`Record`] to a file, one
//! line per run. A line carries where the simulation stops matching the
//! reference emulator and what the run cost, together with the commit,
//! the server and the machine that measured it.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One differential run.
///
/// Every field but `commit` may be absent: a line the nightly writes for a
/// commit that did not build or run carries `error` and nothing measured.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Record {
    /// The commit the run was built from, as `git rev-parse HEAD` printed it.
    pub commit: String,
    /// `SELECT version()` on the server the run used.
    pub clickhouse: Option<String>,
    /// The CPU model of the machine the driver ran on.
    pub runner_cpu: Option<String>,
    /// The first tic `native_state` marks unresolved or unimplemented.
    pub first_refused_tic: Option<u32>,
    /// The column that tic set and the bits it named, as
    /// `unresolved: CHASE_STUCK`.
    pub first_refused_bits: Option<String>,
    /// The first tic a field differs on. Absent when the fields agree or
    /// when nothing was compared, which `compared_through` tells apart.
    pub first_divergent_tic: Option<u32>,
    /// That field, as `mobj slot 1 m_momx`.
    pub first_divergent_field: Option<String>,
    /// The last tic the field comparison covered. Absent when the run
    /// stopped at a refusal before comparing.
    pub compared_through: Option<u32>,
    /// How many tics the comparison covered: the ones both sides hold.
    pub compared_tics: Option<u64>,
    /// `QueryAnalysisMicroseconds` of the simulation's first statement.
    pub stage1_analysis_s: Option<f64>,
    /// The same for its second statement.
    pub stage2_analysis_s: Option<f64>,
    /// The median tic, from its row being sent to its state row being
    /// readable, over every tic but the first, which pays for the analysis.
    pub tic_ms_p50: Option<f64>,
    /// The 95th percentile of the same.
    pub tic_ms_p95: Option<f64>,
    /// Tics the run fed.
    pub tics: Option<u32>,
    /// `GITHUB_RUN_ID`, when the run is a GitHub Actions job.
    pub run_id: Option<String>,
    /// Why the run produced nothing, for a line the nightly writes itself.
    pub error: Option<String>,
}

/// Anything that stops a record from being written or read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} line {line}: {source}")]
    Parse {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Appends `record` to `path` as one line, creating the file and its
/// directory if they are missing.
pub fn append(path: &Path, record: &Record) -> Result<(), Error> {
    let write = |source| Error::Write {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(write)?;
    }
    let line = serde_json::to_string(record).expect("a record serializes");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(write)?;
    writeln!(file, "{line}").map_err(write)
}

/// Every record in `path`, in file order. Blank lines are skipped.
pub fn read(path: &Path) -> Result<Vec<Record>, Error> {
    let text = std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_owned(),
        source,
    })?;
    parse(path, &text)
}

fn parse(path: &Path, text: &str) -> Result<Vec<Record>, Error> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(at, line)| {
            serde_json::from_str(line).map_err(|source| Error::Parse {
                path: path.to_owned(),
                line: at + 1,
                source,
            })
        })
        .collect()
}

/// `git rev-parse HEAD` in the working directory, or `None` outside a
/// checkout.
pub fn commit() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let sha = String::from_utf8(output.stdout).ok()?;
    Some(sha.trim().to_owned()).filter(|sha| !sha.is_empty())
}

/// The CPU model this process runs on: `/proc/cpuinfo`'s `model name` on
/// Linux, `machdep.cpu.brand_string` on macOS, `None` elsewhere.
pub fn cpu_model() -> Option<String> {
    let model = match std::env::consts::OS {
        "linux" => std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|info| cpuinfo_model(&info)),
        "macos" => std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok()),
        _ => None,
    }?;
    Some(model.trim().to_owned()).filter(|model| !model.is_empty())
}

fn cpuinfo_model(info: &str) -> Option<String> {
    info.lines()
        .find_map(|line| line.strip_prefix("model name"))
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, model)| model.trim().to_owned())
}

/// `GITHUB_RUN_ID`, when it is set and not empty.
pub fn run_id() -> Option<String> {
    std::env::var("GITHUB_RUN_ID")
        .ok()
        .filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_round_trips_through_its_line() {
        let record = Record {
            commit: "abc".into(),
            first_refused_tic: Some(275),
            first_refused_bits: Some("unresolved: CHASE_STUCK".into()),
            stage1_analysis_s: Some(40.5),
            ..Record::default()
        };
        let line = serde_json::to_string(&record).unwrap();
        assert!(line.contains("\"first_divergent_tic\":null"), "{line}");
        let back = parse(Path::new("t"), &format!("{line}\n\n{line}\n")).unwrap();
        assert_eq!(back, vec![record.clone(), record]);
    }

    /// The nightly writes an error line with the commit and the reason
    /// alone, and it has to read back.
    #[test]
    fn a_line_carrying_only_the_commit_and_an_error_parses() {
        let back = parse(Path::new("t"), r#"{"commit":"abc","error":"build failed"}"#).unwrap();
        assert_eq!(back[0].commit, "abc");
        assert_eq!(back[0].error.as_deref(), Some("build failed"));
        assert_eq!(back[0].first_refused_tic, None);
    }

    #[test]
    fn a_bad_line_is_named_by_its_number() {
        let err = parse(Path::new("h.jsonl"), "{\"commit\":\"a\"}\nnot json\n").unwrap_err();
        assert!(err.to_string().starts_with("h.jsonl line 2:"), "{err}");
    }

    #[test]
    fn the_cpu_model_is_the_first_model_name_line() {
        let info = "processor\t: 0\nvendor_id\t: AuthenticAMD\n\
                    model name\t: AMD EPYC 7763 64-Core Processor\n\
                    processor\t: 1\nmodel name\t: other\n";
        assert_eq!(
            cpuinfo_model(info).as_deref(),
            Some("AMD EPYC 7763 64-Core Processor")
        );
        assert_eq!(cpuinfo_model("processor\t: 0\n"), None);
    }
}
