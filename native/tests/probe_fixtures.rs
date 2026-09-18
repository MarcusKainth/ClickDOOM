//! Every committed probe row set carries the contract's fields.
//!
//! A probe row is three leading fields, `frame_index`, `gametic` and
//! `fb_hash`, then one field per entry of
//! `clickdoom_spec::native_state::all_fields`. The staging table
//! `support::probe` builds takes its column list from `native_state`, so a
//! row set written against an older field list fails to parse rather than
//! loading crooked, and it fails inside whichever suite happens to load it.
//! This names the file instead.
//!
//! The directories are scanned rather than listed, so a row set added later
//! is covered without being added here.

use std::path::{Path, PathBuf};

/// Where probe row sets are committed, relative to the repository root.
const FIXTURE_DIRS: [&str; 2] = ["native/tests/fixtures", "refemu/probe/fixtures"];

/// The three fields a probe row carries ahead of the state row.
const LEADING: usize = 3;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("native/ has a parent")
        .to_path_buf()
}

/// Every `.tsv` under the fixture directories, sorted.
fn probe_tsvs() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = FIXTURE_DIRS
        .iter()
        .flat_map(|dir| {
            let path = repo_root().join(dir);
            std::fs::read_dir(&path)
                .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()))
                .map(|entry| entry.expect("a directory entry").path())
                .filter(|p| p.extension().is_some_and(|e| e == "tsv"))
                .collect::<Vec<_>>()
        })
        .collect();
    found.sort();
    found
}

#[test]
fn every_committed_probe_row_carries_the_contracts_fields() {
    let want = LEADING + clickdoom_spec::native_state::all_fields().len();
    let files = probe_tsvs();
    assert!(!files.is_empty(), "no probe row sets found to check");
    for path in files {
        let text = std::fs::read_to_string(&path).expect("the fixture reads");
        let name = path.display();
        let mut rows = 0;
        for (line_number, line) in text.lines().enumerate() {
            if let Some(header) = line.strip_prefix("# columns\t") {
                let columns: Vec<&str> = header.split('\t').collect();
                assert_eq!(
                    &columns[LEADING..],
                    clickdoom_spec::native_state::all_fields().as_slice(),
                    "{name} names a different field list"
                );
                continue;
            }
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            rows += 1;
            assert_eq!(
                line.split('\t').count(),
                want,
                "{name} line {} carries the wrong number of fields",
                line_number + 1
            );
        }
        assert!(rows > 0, "{name} carries no rows");
    }
}
