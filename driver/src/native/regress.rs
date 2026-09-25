//! Whether a recorded differential run regressed.
//!
//! [`judge`] walks new [`Record`]s in order, each against what came before
//! it. Correctness is judged against the latest line with no error, in the
//! history or earlier in the new lines. Cost is judged only against the
//! line directly before it in the new lines, and only when both were
//! measured in the same run on the same CPU model and server, because an
//! analysis time taken on another machine says nothing about this change.

use std::collections::HashSet;

use serde::Serialize;

use super::record::Record;

/// How much slower a child may be than its parent before it counts.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Child over parent for `stage1_analysis_s`.
    pub analysis_ratio: f64,
    /// Child over parent for `tic_ms_p50`.
    pub tic_ratio: f64,
}

/// Which kind of regression a [`Finding`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Correctness,
    Cost,
}

/// One metric that got worse from one line to the next.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub kind: Kind,
    /// The line that got worse.
    pub commit: String,
    /// The line it was judged against.
    pub against: String,
    /// The [`Record`] field that moved.
    pub metric: &'static str,
    pub before: String,
    pub after: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            Kind::Correctness => "correctness",
            Kind::Cost => "cost",
        };
        write!(
            f,
            "{kind}: {} {} against {} at {}",
            self.metric,
            self.after,
            self.before,
            short(&self.against)
        )
    }
}

/// What [`judge`] made of one new line.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// The commit already has a line in the history, or earlier in the new
    /// lines. It is not judged again, and serves as the next line's parent.
    Recorded { commit: String },
    /// The line carries an error, so there is nothing to judge.
    Error { commit: String, error: String },
    Judged {
        commit: String,
        /// The line correctness was judged against, if any came before.
        against: Option<String>,
        /// The parent cost was judged against, if one was measured in the
        /// same run on the same machine.
        cost_against: Option<String>,
        findings: Vec<Finding>,
    },
}

/// Judges every line of `new`, in order, against `history` and the lines
/// of `new` before it.
pub fn judge(history: &[Record], new: &[Record], limits: Limits) -> Vec<Verdict> {
    let mut seen: HashSet<&str> = history.iter().map(|line| line.commit.as_str()).collect();
    let mut baseline = history.iter().rev().find(|line| line.error.is_none());
    let mut parent: Option<&Record> = None;
    let mut verdicts = Vec::with_capacity(new.len());
    for line in new {
        if let Some(error) = &line.error {
            verdicts.push(Verdict::Error {
                commit: line.commit.clone(),
                error: error.clone(),
            });
            parent = None;
            continue;
        }
        let cost_parent = parent.filter(|parent| same_machine(parent, line));
        if !seen.insert(line.commit.as_str()) {
            verdicts.push(Verdict::Recorded {
                commit: line.commit.clone(),
            });
        } else {
            let mut findings = baseline.map_or_else(Vec::new, |base| correctness(base, line));
            if let Some(parent) = cost_parent {
                findings.extend(cost(parent, line, limits));
            }
            verdicts.push(Verdict::Judged {
                commit: line.commit.clone(),
                against: baseline.map(|base| base.commit.clone()),
                cost_against: cost_parent.map(|parent| parent.commit.clone()),
                findings,
            });
        }
        baseline = Some(line);
        parent = Some(line);
    }
    verdicts
}

/// Both lines were measured by one run on one CPU model and one server.
fn same_machine(a: &Record, b: &Record) -> bool {
    a.run_id.is_some()
        && a.run_id == b.run_id
        && a.runner_cpu.is_some()
        && a.runner_cpu == b.runner_cpu
        && a.clickhouse == b.clickhouse
}

/// The first refusal moves earlier, appears, or names other bits at the
/// same tic; or a divergence appears or moves earlier. A refusal that
/// moves later is progress, and the bits it names then are new ones.
fn correctness(base: &Record, line: &Record) -> Vec<Finding> {
    let finding = |metric, before: String, after: String| Finding {
        kind: Kind::Correctness,
        commit: line.commit.clone(),
        against: base.commit.clone(),
        metric,
        before,
        after,
    };
    let mut findings = Vec::new();
    match (base.first_refused_tic, line.first_refused_tic) {
        (b, Some(l)) if b.is_none_or(|b| l < b) => {
            findings.push(finding("first_refused_tic", show(b), l.to_string()));
        }
        (Some(b), Some(l)) if b == l && base.first_refused_bits != line.first_refused_bits => {
            findings.push(finding(
                "first_refused_bits",
                show(base.first_refused_bits.as_deref()),
                show(line.first_refused_bits.as_deref()),
            ));
        }
        _ => {}
    }
    // A line that compared nothing says nothing about divergence.
    if base.compared_through.is_some() && line.compared_through.is_some() {
        match (base.first_divergent_tic, line.first_divergent_tic) {
            (b, Some(l)) if b.is_none_or(|b| l < b) => findings.push(finding(
                "first_divergent_tic",
                divergence(b, base),
                divergence(Some(l), line),
            )),
            _ => {}
        }
    }
    findings
}

/// `stage1_analysis_s` or `tic_ms_p50` over its limit against the parent.
fn cost(parent: &Record, line: &Record, limits: Limits) -> Vec<Finding> {
    let metrics = [
        (
            "stage1_analysis_s",
            parent.stage1_analysis_s,
            line.stage1_analysis_s,
            limits.analysis_ratio,
        ),
        (
            "tic_ms_p50",
            parent.tic_ms_p50,
            line.tic_ms_p50,
            limits.tic_ratio,
        ),
    ];
    metrics
        .into_iter()
        .filter_map(|(metric, before, after, ratio)| {
            let (before, after) = (before?, after?);
            (before > 0.0 && after > before * ratio).then(|| Finding {
                kind: Kind::Cost,
                commit: line.commit.clone(),
                against: parent.commit.clone(),
                metric,
                before: format!("{before:.3}"),
                after: format!("{after:.3} ({:.2}x)", after / before),
            })
        })
        .collect()
}

fn divergence(tic: Option<u32>, line: &Record) -> String {
    match (tic, &line.first_divergent_field) {
        (Some(tic), Some(field)) => format!("{tic} ({field})"),
        (Some(tic), None) => tic.to_string(),
        (None, _) => format!("none through {}", show(line.compared_through)),
    }
}

fn show<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "none".to_owned(), |value| value.to_string())
}

/// The first 12 hex digits of a commit, as the summary prints it.
pub fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        analysis_ratio: 1.25,
        tic_ratio: 1.3,
    };

    /// History and new lines as the nightly writes them: JSON, one per line.
    fn lines(text: &str) -> Vec<Record> {
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("a fixture line parses"))
            .collect()
    }

    const HISTORY: &str = r#"
{"commit":"a1","clickhouse":"26.8.2.7","runner_cpu":"EPYC","run_id":"1","first_refused_tic":275,"first_refused_bits":"unresolved: CHASE_STUCK","compared_through":274,"compared_tics":273,"stage1_analysis_s":180.0,"tic_ms_p50":500.0}
{"commit":"a2","error":"cargo build failed"}
"#;

    fn findings(verdict: &Verdict) -> &[Finding] {
        match verdict {
            Verdict::Judged { findings, .. } => findings,
            other => panic!("not judged: {other:?}"),
        }
    }

    #[test]
    fn a_refusal_that_moves_earlier_is_a_regression() {
        let new = lines(
            r#"{"commit":"b1","clickhouse":"26.8.2.7","runner_cpu":"EPYC","run_id":"2","first_refused_tic":210,"first_refused_bits":"unresolved: PL_ACTION_NEEDED","compared_through":209,"stage1_analysis_s":180.0,"tic_ms_p50":500.0}"#,
        );
        let verdicts = judge(&lines(HISTORY), &new, LIMITS);
        assert_eq!(verdicts.len(), 1);
        let Verdict::Judged {
            against,
            cost_against,
            findings,
            ..
        } = &verdicts[0]
        else {
            panic!("{verdicts:?}");
        };
        assert_eq!(against.as_deref(), Some("a1"), "error lines are skipped");
        assert_eq!(*cost_against, None, "no parent measured in this run");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].kind, Kind::Correctness);
        assert_eq!(findings[0].metric, "first_refused_tic");
        assert_eq!(
            (findings[0].before.as_str(), findings[0].after.as_str()),
            ("275", "210")
        );
    }

    #[test]
    fn a_refusal_that_moves_later_is_progress_whatever_bits_it_names() {
        let new = lines(
            r#"{"commit":"b1","first_refused_tic":300,"first_refused_bits":"unresolved: PX_CROSSED","compared_through":299}"#,
        );
        assert_eq!(findings(&judge(&lines(HISTORY), &new, LIMITS)[0]), &[]);
    }

    #[test]
    fn other_bits_at_the_same_tic_are_a_regression() {
        let new = lines(
            r#"{"commit":"b1","first_refused_tic":275,"first_refused_bits":"unresolved: PX_CROSSED","compared_through":274}"#,
        );
        let verdicts = judge(&lines(HISTORY), &new, LIMITS);
        let found = findings(&verdicts[0]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].metric, "first_refused_bits");
        assert_eq!(found[0].after, "unresolved: PX_CROSSED");
    }

    #[test]
    fn a_divergence_that_appears_or_moves_earlier_is_a_regression() {
        let new = lines(
            r#"
{"commit":"b1","first_refused_tic":275,"first_refused_bits":"unresolved: CHASE_STUCK","compared_through":274,"first_divergent_tic":206,"first_divergent_field":"mobj slot 1 m_momx"}
{"commit":"b2","first_refused_tic":275,"first_refused_bits":"unresolved: CHASE_STUCK","compared_through":274,"first_divergent_tic":206,"first_divergent_field":"mobj slot 1 m_momy"}
{"commit":"b3","first_refused_tic":275,"first_refused_bits":"unresolved: CHASE_STUCK","compared_through":274,"first_divergent_tic":100,"first_divergent_field":"player mo m_x"}
"#,
        );
        let verdicts = judge(&lines(HISTORY), &new, LIMITS);
        let appears = findings(&verdicts[0]);
        assert_eq!(appears.len(), 1, "{appears:?}");
        assert_eq!(appears[0].metric, "first_divergent_tic");
        assert_eq!(appears[0].before, "none through 274");
        assert_eq!(appears[0].after, "206 (mobj slot 1 m_momx)");
        assert_eq!(findings(&verdicts[1]), &[], "the same tic is not earlier");
        let earlier = findings(&verdicts[2]);
        assert_eq!(earlier.len(), 1, "{earlier:?}");
        assert_eq!(earlier[0].against, "b2", "judged against the line before");
    }

    /// Cost is judged only against a parent measured by the same run on the
    /// same machine, never against history from another runner.
    #[test]
    fn cost_is_judged_against_the_parent_measured_in_the_same_run() {
        let same = r#""clickhouse":"26.8.2.7","runner_cpu":"EPYC","run_id":"2","first_refused_tic":275,"first_refused_bits":"unresolved: CHASE_STUCK","compared_through":274"#;
        let new = lines(&format!(
            "{{\"commit\":\"a1\",{same},\"stage1_analysis_s\":100.0,\"tic_ms_p50\":400.0}}\n\
             {{\"commit\":\"b1\",{same},\"stage1_analysis_s\":130.0,\"tic_ms_p50\":510.0}}\n\
             {{\"commit\":\"b2\",{same},\"stage1_analysis_s\":140.0,\"tic_ms_p50\":520.0}}\n\
             {{\"commit\":\"b3\",\"clickhouse\":\"26.8.2.7\",\"runner_cpu\":\"Xeon\",\"run_id\":\"2\",\"first_refused_tic\":275,\"first_refused_bits\":\"unresolved: CHASE_STUCK\",\"compared_through\":274,\"stage1_analysis_s\":900.0,\"tic_ms_p50\":900.0}}"
        ));
        let verdicts = judge(&lines(HISTORY), &new, LIMITS);
        assert_eq!(
            verdicts[0],
            Verdict::Recorded {
                commit: "a1".into()
            },
            "a commit the history holds is the next one's parent, not judged again"
        );
        let slower = findings(&verdicts[1]);
        assert_eq!(slower.len(), 1, "{slower:?}");
        assert_eq!(slower[0].kind, Kind::Cost);
        assert_eq!(slower[0].metric, "stage1_analysis_s");
        assert_eq!(slower[0].after, "130.000 (1.30x)");
        assert_eq!(findings(&verdicts[2]), &[], "within both limits of b1");
        let Verdict::Judged { cost_against, .. } = &verdicts[3] else {
            panic!("{verdicts:?}");
        };
        assert_eq!(*cost_against, None, "another CPU model is not a parent");
    }

    #[test]
    fn an_error_line_is_reported_and_breaks_the_cost_chain() {
        let same =
            r#""clickhouse":"26.8.2.7","runner_cpu":"EPYC","run_id":"2","compared_through":274"#;
        let new = lines(&format!(
            "{{\"commit\":\"b1\",{same},\"stage1_analysis_s\":100.0}}\n\
             {{\"commit\":\"b2\",\"error\":\"the diff exited 1\"}}\n\
             {{\"commit\":\"b3\",{same},\"stage1_analysis_s\":900.0}}"
        ));
        let verdicts = judge(&[], &new, LIMITS);
        assert_eq!(
            verdicts[1],
            Verdict::Error {
                commit: "b2".into(),
                error: "the diff exited 1".into()
            }
        );
        let Verdict::Judged {
            against,
            cost_against,
            findings,
            ..
        } = &verdicts[2]
        else {
            panic!("{verdicts:?}");
        };
        assert_eq!(against.as_deref(), Some("b1"));
        assert_eq!(*cost_against, None, "b3's parent errored");
        assert_eq!(findings, &[]);
    }

    #[test]
    fn a_line_that_compared_nothing_says_nothing_about_divergence() {
        let new = lines(
            r#"
{"commit":"b1","first_refused_tic":275,"first_refused_bits":"unresolved: CHASE_STUCK"}
{"commit":"b2","first_refused_tic":275,"first_refused_bits":"unresolved: CHASE_STUCK","compared_through":274,"first_divergent_tic":5}
"#,
        );
        let verdicts = judge(&[], &new, LIMITS);
        let Verdict::Judged { against, .. } = &verdicts[0] else {
            panic!("{verdicts:?}");
        };
        assert_eq!(*against, None, "an empty history judges nothing");
        assert_eq!(findings(&verdicts[1]), &[]);
    }
}
