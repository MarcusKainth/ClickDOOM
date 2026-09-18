//! Every simulation suite is selected by exactly one CI group.
//!
//! `scripts/test-group.sh` splits the simulation suites across the lettered
//! groups by name, and one group takes everything the others do not name.
//! A suite named by no group would run nowhere and CI would stay green
//! without it; a suite named by two would run twice and cost a group its
//! budget. Neither shows up in a run's own output, so this reads the
//! script's own filters and the suites on disk and says which.
//!
//! This parses the `binary(...)` names out of the `sim_<letter>=` lines
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

/// The suites each `sim_<letter>=` line names, by letter.
fn named_by_letter(script: &str) -> BTreeMap<char, Vec<String>> {
    let mut named = BTreeMap::new();
    for line in script.lines() {
        let Some(rest) = line.strip_prefix("sim_") else {
            continue;
        };
        let mut chars = rest.chars();
        let (Some(letter), Some('=')) = (chars.next(), chars.next()) else {
            continue;
        };
        let body = rest[2..].trim_matches('\'');
        let mut suites = Vec::new();
        for term in body.split('|') {
            let term = term.trim();
            let inner = term
                .strip_prefix("binary(")
                .and_then(|t| t.strip_suffix(')'))
                .unwrap_or_else(|| {
                    panic!("sim_{letter} holds {term}, which is not a plain binary() term")
                });
            suites.push(inner.to_owned());
        }
        named.insert(letter, suites);
    }
    assert!(!named.is_empty(), "the script names no lettered group");
    named
}

/// The letter whose case runs everything the others do not name.
fn catch_all(script: &str) -> char {
    let mut found: Option<char> = None;
    let mut case: Option<char> = None;
    for line in script.lines() {
        if let Some(rest) = line
            .trim()
            .strip_prefix("native-sim-")
            .filter(|rest| rest.trim_end().ends_with(')'))
        {
            case = rest.chars().next();
        }
        if line.contains("binary(/^sim_/) and not (") {
            let letter = case.expect("the catch-all filter sits inside a native-sim-<letter> case");
            assert!(found.is_none(), "two groups claim to be the catch-all");
            found = Some(letter);
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
    let named = named_by_letter(&script);
    let rest = catch_all(&script);
    assert!(
        !named.contains_key(&rest),
        "native-sim-{rest} both names suites and takes the rest",
    );

    let mut groups: BTreeMap<String, Vec<char>> = BTreeMap::new();
    for suite in suites_on_disk() {
        groups.entry(suite).or_default();
    }
    for (letter, suites) in &named {
        for suite in suites {
            groups
                .get_mut(suite)
                .unwrap_or_else(|| panic!("sim_{letter} names {suite}, which is not on disk"))
                .push(*letter);
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
