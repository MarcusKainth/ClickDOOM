//! `clickdoom native regress`: whether recorded differential runs got
//! worse.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Args;

use crate::cli::{Exit, Failure, failed, gate};
use crate::native::record::{self, Record};
use crate::native::regress::{self, Finding, Limits, Verdict, short};

#[derive(Args)]
#[command(
    about = "Judge `native diff --record` lines against the ones before them",
    // Hard-wrapped: clap only rewraps help text with its `wrap_help`
    // feature, which this binary does not enable.
    long_about = "\
Read the lines `native diff --record` wrote to NEW and judge each one, in
order, against the lines before it. Talks to no server.

Correctness is judged against the latest line with no error, in --history
or earlier in NEW. It regressed when the first refused tic moves earlier or
appears, when it names other bits at the same tic, or when a divergence
appears or moves earlier. A refusal that moves later is progress.

Cost is judged only against the line directly before it in NEW, and only
when both were measured by the same run (run_id) on the same CPU model and
server. It regressed when stage1_analysis_s is over --analysis-ratio times
the parent's, or tic_ms_p50 over --tic-ratio times. A line for a commit
already in --history is not judged again and serves as the next line's
parent, so a run that measures the last recorded commit first can judge
the cost of the one after it.

Exit codes: 0 nothing regressed, 1 a file could not be read or NEW holds no
line, 3 something regressed."
)]
pub struct RegressCmd {
    /// The lines to judge, in commit order
    #[arg(value_name = "NEW")]
    pub new: PathBuf,
    /// Lines already judged, oldest first
    #[arg(long, value_name = "PATH")]
    pub history: Option<PathBuf>,
    /// Write every regression to this file as one JSON line
    #[arg(long, value_name = "PATH")]
    pub findings: Option<PathBuf>,
    /// How much slower stage1's analysis may be than the parent's
    #[arg(long, default_value_t = 1.25, value_name = "RATIO")]
    pub analysis_ratio: f64,
    /// How much slower the median tic may be than the parent's
    #[arg(long, default_value_t = 1.3, value_name = "RATIO")]
    pub tic_ratio: f64,
}

pub(crate) fn run(cmd: &RegressCmd) -> Result<Exit, Failure> {
    let read = |path: &Path| record::read(path).map_err(|err| failed(err.to_string()));
    let history = match &cmd.history {
        Some(path) => read(path)?,
        None => Vec::new(),
    };
    let new: Vec<Record> = read(&cmd.new)?;
    if new.is_empty() {
        return Err(failed(format!(
            "{} holds no line, so nothing was judged",
            cmd.new.display()
        )));
    }
    let limits = Limits {
        analysis_ratio: cmd.analysis_ratio,
        tic_ratio: cmd.tic_ratio,
    };
    let verdicts = regress::judge(&history, &new, limits);
    let findings: Vec<&Finding> = verdicts.iter().flat_map(findings).collect();
    for verdict in &verdicts {
        println!("{}", summary(verdict));
    }
    if let Some(path) = &cmd.findings {
        write_findings(path, &findings)?;
    }
    if findings.is_empty() {
        return Ok(Exit::Ok);
    }
    Err(gate(format!(
        "{} regression(s) over {} line(s)",
        findings.len(),
        new.len()
    )))
}

fn findings(verdict: &Verdict) -> &[Finding] {
    match verdict {
        Verdict::Judged { findings, .. } => findings,
        _ => &[],
    }
}

/// One line per verdict, and one more per finding.
fn summary(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Recorded { commit } => {
            format!("{} already recorded, not judged again", short(commit))
        }
        Verdict::Error { commit, error } => format!("{} not judged: {error}", short(commit)),
        Verdict::Judged {
            commit,
            against,
            cost_against,
            findings,
        } => {
            let against = against
                .as_deref()
                .map_or("nothing before it".to_owned(), |c| short(c).to_owned());
            let cost = cost_against.as_deref().map_or(
                "cost not judged: no parent measured by this run on this machine".to_owned(),
                |c| format!("cost against {}", short(c)),
            );
            let mut text = format!(
                "{} against {against}, {cost}: {}",
                short(commit),
                match findings.len() {
                    0 => "no regression".to_owned(),
                    n => format!("{n} regression(s)"),
                }
            );
            for finding in findings {
                text.push_str(&format!("\n  {finding}"));
            }
            text
        }
    }
}

fn write_findings(path: &Path, findings: &[&Finding]) -> Result<(), Failure> {
    let write = |err: std::io::Error| failed(format!("writing {}: {err}", path.display()));
    let mut file = std::fs::File::create(path).map_err(write)?;
    for finding in findings {
        let line = serde_json::to_string(finding).expect("a finding serializes");
        writeln!(file, "{line}").map_err(write)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(new: &Path, history: Option<&Path>, findings: Option<&Path>) -> RegressCmd {
        RegressCmd {
            new: new.to_owned(),
            history: history.map(Path::to_owned),
            findings: findings.map(Path::to_owned),
            analysis_ratio: 1.25,
            tic_ratio: 1.3,
        }
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/regress")
            .join(name)
    }

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("clickdoom-regress-{}-{name}", std::process::id()))
    }

    /// A history whose last line refuses at 275, and a night whose second
    /// commit refuses at 210: the command exits 3 and writes that finding.
    #[test]
    fn a_refusal_that_moved_earlier_exits_3_and_is_written() {
        let out = scratch("moved.jsonl");
        let result = run(&cmd(
            &fixture("night-moved.jsonl"),
            Some(&fixture("history.jsonl")),
            Some(&out),
        ));
        let Err(failure) = result else {
            panic!("a moved refusal must be a regression");
        };
        assert_eq!(failure.exit, Exit::Gate, "{}", failure.message);
        let written = std::fs::read_to_string(&out).unwrap();
        std::fs::remove_file(&out).ok();
        let lines: Vec<serde_json::Value> = written
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 1, "{written}");
        assert_eq!(lines[0]["kind"], "correctness");
        assert_eq!(lines[0]["metric"], "first_refused_tic");
        assert_eq!(lines[0]["before"], "275");
        assert_eq!(lines[0]["after"], "210");
        assert_eq!(
            lines[0]["commit"],
            "2222222222222222222222222222222222222222"
        );
    }

    #[test]
    fn a_night_that_held_steady_exits_0_and_writes_no_finding() {
        let out = scratch("steady.jsonl");
        let result = run(&cmd(
            &fixture("night-steady.jsonl"),
            Some(&fixture("history.jsonl")),
            Some(&out),
        ));
        let written = std::fs::read_to_string(&out).unwrap();
        std::fs::remove_file(&out).ok();
        assert!(matches!(result, Ok(Exit::Ok)));
        assert_eq!(written, "");
    }

    /// A night that judged nothing is a failure, not a pass.
    #[test]
    fn an_empty_night_fails() {
        let empty = scratch("empty.jsonl");
        std::fs::write(&empty, "\n").unwrap();
        let result = run(&cmd(&empty, None, None));
        std::fs::remove_file(&empty).ok();
        let Err(failure) = result else {
            panic!("an empty file judged nothing");
        };
        assert_eq!(failure.exit, Exit::Failed);
        assert!(
            failure.message.contains("nothing was judged"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn a_missing_history_fails_rather_than_judging_against_nothing() {
        let result = run(&cmd(
            &fixture("night-steady.jsonl"),
            Some(&fixture("absent.jsonl")),
            None,
        ));
        assert!(matches!(
            result,
            Err(Failure {
                exit: Exit::Failed,
                ..
            })
        ));
    }
}
