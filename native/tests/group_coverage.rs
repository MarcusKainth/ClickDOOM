//! Every simulation suite is selected by exactly one CI group, and CI runs
//! every group the script defines.
//!
//! `scripts/test-group.sh` splits the simulation suites across the named
//! groups, and one group takes everything the others do not name. A suite
//! named by no group would run nowhere and CI would stay green without it;
//! a suite named by two would run twice and cost a group its budget.
//! Neither shows up in a run's own output, so this reads the script's own
//! filters and the suites on disk and says which. `test-group.sh --check`
//! makes the same check over the built archives, for every package.
//!
//! This parses the `binary(...)` names out of the `sim_<name>=` lines
//! rather than evaluating a filterset, so a filter that stops being a
//! plain list of `binary()` terms fails here rather than being read wrong.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("native/ has a parent")
        .to_path_buf()
}

/// The suites on disk: one per `native/tests/sim_*_live.rs`.
fn suites_on_disk() -> Vec<String> {
    let dir = repo_root().join("native/tests");
    let mut found: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter_map(|p| {
            let name = p.file_stem()?.to_str()?.to_owned();
            (p.extension()? == "rs" && name.starts_with("sim_") && name.ends_with("_live"))
                .then_some(name)
        })
        .collect();
    found.sort();
    found
}

/// The suites each `sim_<name>=` line names, by name.
fn named_by_group(script: &str) -> BTreeMap<String, Vec<String>> {
    let mut named = BTreeMap::new();
    for line in script.lines() {
        let Some((name, body)) = line
            .strip_prefix("sim_")
            .and_then(|rest| rest.split_once('='))
            .filter(|(name, _)| name.chars().all(|c| c.is_ascii_alphanumeric()))
        else {
            continue;
        };
        let body = body.trim_matches('\'');
        let mut suites = Vec::new();
        for term in body.split('|') {
            let term = term.trim();
            let inner = term
                .strip_prefix("binary(")
                .and_then(|t| t.strip_suffix(')'))
                .unwrap_or_else(|| {
                    panic!("sim_{name} holds {term}, which is not a plain binary() term")
                });
            suites.push(inner.to_owned());
        }
        named.insert(name.to_owned(), suites);
    }
    assert!(!named.is_empty(), "the script names no simulation group");
    named
}

/// The label of each `case` arm that names a group, in order. Arms for
/// options (`--check`) and the fallback (`*`) are left out.
fn case_labels(script: &str) -> Vec<String> {
    script
        .lines()
        .filter_map(|line| {
            let label = line.strip_prefix("    ")?.strip_suffix(')')?;
            (label.starts_with(|c: char| c.is_ascii_alphanumeric())
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
            .then(|| label.to_owned())
        })
        .collect()
}

/// The name whose case runs everything the others do not name.
fn catch_all(script: &str) -> String {
    let mut found: Option<String> = None;
    let mut case: Option<String> = None;
    for line in script.lines() {
        if let Some(name) = line
            .strip_prefix("    native-sim-")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            case = Some(name.to_owned());
        }
        if line.contains("binary(/^sim_/) and not (") {
            let name = case
                .clone()
                .expect("the catch-all filter sits inside a native-sim-<name> case");
            assert!(found.is_none(), "two groups claim to be the catch-all");
            found = Some(name);
        }
    }
    found.expect(
        "no group takes the suites the others do not name, so a new suite would run nowhere",
    )
}

#[test]
fn every_simulation_suite_runs_in_exactly_one_group() {
    let script = std::fs::read_to_string(repo_root().join("scripts/test-group.sh"))
        .expect("the script reads");
    let named = named_by_group(&script);
    let rest = catch_all(&script);
    assert!(
        !named.contains_key(&rest),
        "native-sim-{rest} both names suites and takes the rest",
    );

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for suite in suites_on_disk() {
        groups.entry(suite).or_default();
    }
    for (name, suites) in &named {
        for suite in suites {
            groups
                .get_mut(suite)
                .unwrap_or_else(|| panic!("sim_{name} names {suite}, which is not on disk"))
                .push(name.clone());
        }
    }

    let twice: Vec<String> = groups
        .iter()
        .filter(|(_, ls)| ls.len() > 1)
        .map(|(s, ls)| format!("{s} in {ls:?}"))
        .collect();
    assert!(twice.is_empty(), "named by more than one group: {twice:?}");

    // Everything named by nobody falls to the catch-all, which is where a
    // new suite lands until somebody packs it. That is not a failure; a
    // group that does not exist would be.
    let unnamed: Vec<&String> = groups
        .iter()
        .filter(|(_, ls)| ls.is_empty())
        .map(|(s, _)| s)
        .collect();
    assert!(
        !unnamed.is_empty() || named.values().map(Vec::len).sum::<usize>() == groups.len(),
        "every suite is named, so native-sim-{rest} runs nothing and its budget is wasted"
    );
}

/// The `groups=(...)` list, which `--check` walks and the usage line prints.
fn listed_groups(script: &str) -> Vec<String> {
    let line = script
        .lines()
        .find_map(|line| line.strip_prefix("groups=("))
        .and_then(|rest| rest.strip_suffix(')'))
        .expect("the script lists its groups on one `groups=(...)` line");
    line.split_whitespace().map(str::to_owned).collect()
}

/// The groups every `group: [...]` line of ci.yml's test matrices lists.
fn matrix_groups(workflow: &str) -> Vec<String> {
    let lines: Vec<&str> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("group: ["))
        .map(|rest| {
            rest.strip_suffix(']')
                .expect("a test matrix lists its groups on one `group: [...]` line")
        })
        .collect();
    assert!(!lines.is_empty(), "ci.yml has no `group: [...]` line");
    lines
        .iter()
        .flat_map(|line| line.split(','))
        .map(|g| g.trim().to_owned())
        .collect()
}

#[test]
fn ci_runs_every_group_the_script_defines() {
    let script = std::fs::read_to_string(repo_root().join("scripts/test-group.sh"))
        .expect("the script reads");
    let workflow = std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml"))
        .expect("the workflow reads");

    let mut listed = listed_groups(&script);
    let mut cases = case_labels(&script);
    let mut matrix = matrix_groups(&workflow);
    assert!(
        listed.len() > 1,
        "the groups line was read as {listed:?}, which cannot be right"
    );
    for names in [&mut listed, &mut cases, &mut matrix] {
        names.sort();
    }
    assert_eq!(
        cases, listed,
        "the script's case arms and its groups line name different groups"
    );
    assert_eq!(
        matrix, listed,
        "ci.yml's test matrices and the script's groups line name different groups"
    );
    for (name, _) in named_by_group(&script) {
        assert!(
            listed.contains(&format!("native-sim-{name}")),
            "sim_{name} names suites for native-sim-{name}, which is not a group"
        );
    }
}
