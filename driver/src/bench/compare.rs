//! Comparing throughput across an arbitrary number of ClickHouse versions,
//! head to head on the same ROM. Any number of arms, each named by a
//! caller-chosen label.
//!
//! Every (repeat, arm) pair calls [`crate::bench::canonical::run`]
//! in-process against that arm's freshly started container, rather than
//! shelling out to a separate binary and re-parsing its output. Repeats
//! rotate arm order (`repeat 1: A,B,C`, `repeat 2: B,C,A`, ...) to cancel
//! warm-up bias between arms.

use std::path::{Path, PathBuf};

use clickdoom_spec::sha256_hex;

use super::canonical::{self, Windows};
use super::report::{self, ArmRecord, CompareRecord, WindowRecord};
use crate::client::ConnArgs;

#[derive(Clone, Debug)]
pub struct Arm {
    pub name: String,
    pub image: String,
}

#[derive(Debug, thiserror::Error)]
#[error("'{0}' is not NAME=<docker-image-ref>")]
pub struct ParseArmError(String);

/// Parses one `--arm` flag's value: `NAME=<docker-image-ref>`.
pub fn parse_arm(s: &str) -> Result<Arm, ParseArmError> {
    let (name, image) = s
        .split_once('=')
        .ok_or_else(|| ParseArmError(s.to_string()))?;
    if name.is_empty() || image.is_empty() {
        return Err(ParseArmError(s.to_string()));
    }
    Ok(Arm {
        name: name.to_string(),
        image: image.to_string(),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum CompareError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("starting arm {0} ({1}): {2}")]
    DockerStart(String, String, super::docker::DockerError),
    #[error("arm {0}: {1}")]
    Canonical(String, canonical::CanonicalError),
    #[error(transparent)]
    Db(#[from] crate::client::Error),
}

pub struct Args<'a> {
    pub bin: &'a Path,
    pub manifest_path: &'a Path,
    pub k: u32,
    pub hwm: u32,
    pub repeats: u32,
    pub batches: u32,
    pub windows: Windows,
    pub snapshot_dir: PathBuf,
    pub refemu_bin: PathBuf,
    pub arms: Vec<Arm>,
    pub note: Option<String>,
}

fn rotate(arms: &[Arm], repeat: u32) -> Vec<&Arm> {
    let n = arms.len();
    let shift = (repeat as usize - 1) % n;
    (0..n).map(|i| &arms[(i + shift) % n]).collect()
}

fn merge(totals: &mut Vec<WindowRecord>, report: &canonical::Report) {
    for w in &report.windows {
        if let Some(existing) = totals.iter_mut().find(|t| t.label == w.label) {
            existing.fold_retired += w.fold.retired;
            existing.fold_seconds += w.fold.seconds;
            existing.e2e_retired += w.e2e.retired;
            existing.e2e_seconds += w.e2e.seconds;
        } else {
            totals.push(WindowRecord {
                label: w.label.clone(),
                k: w.k,
                hwm: w.hwm,
                fold_retired: w.fold.retired,
                fold_seconds: w.fold.seconds,
                e2e_retired: w.e2e.retired,
                e2e_seconds: w.e2e.seconds,
            });
        }
    }
}

/// Runs every repeat, in rotated arm order, collecting one summed
/// [`WindowRecord`] set per arm.
pub async fn run(args: &Args<'_>) -> Result<CompareRecord, CompareError> {
    let blob = std::fs::read(args.bin).map_err(|source| CompareError::Read {
        path: args.bin.to_owned(),
        source,
    })?;
    let rom_sha256 = sha256_hex(&blob);
    let git_sha = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default(),
    )
    .unwrap_or_default()
    .trim()
    .to_string();

    let mut totals: Vec<(String, String, Vec<WindowRecord>)> = args
        .arms
        .iter()
        .map(|a| (a.name.clone(), a.image.clone(), Vec::new()))
        .collect();
    let mut versions: Vec<String> = vec![String::new(); args.arms.len()];

    for repeat in 1..=args.repeats {
        let order = rotate(&args.arms, repeat);
        eprintln!(
            "# repeat {repeat}: arms {}",
            order
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        for arm in order {
            let idx = args
                .arms
                .iter()
                .position(|a| a.name == arm.name)
                .expect("arm came from args.arms");
            eprintln!(
                "# repeat {repeat} arm {}: starting fresh container",
                arm.name
            );
            let container = super::docker::start(&arm.image)
                .await
                .map_err(|e| CompareError::DockerStart(arm.name.clone(), arm.image.clone(), e))?;

            let conn = ConnArgs {
                host: "127.0.0.1".to_string(),
                port: container.http_port,
                user: "default".to_string(),
                database: "default".to_string(),
                password: Some("clickdoom".to_string()),
            };
            let db = conn.connect();
            let version: String = db.fetch_one("SELECT version()").await?;
            versions[idx] = version.clone();
            eprintln!(
                "# repeat {repeat} arm {}: version={version} port={}",
                arm.name, container.http_port
            );

            let canonical_args = canonical::Args {
                bin: args.bin,
                manifest_path: args.manifest_path,
                k: args.k,
                hwm: args.hwm,
                batches: args.batches,
                windows: Windows {
                    boot_end_icount: args.windows.boot_end_icount,
                    gameplay_target_icount: args.windows.gameplay_target_icount,
                    gameplay_window_end_icount: args.windows.gameplay_window_end_icount,
                },
                snapshot_dir: args.snapshot_dir.clone(),
                refemu_bin: args.refemu_bin.clone(),
            };
            let result = canonical::run(&conn, &canonical_args).await;
            drop(container);
            let bench_report = result.map_err(|e| CompareError::Canonical(arm.name.clone(), e))?;
            merge(&mut totals[idx].2, &bench_report);
        }
    }

    let arms = totals
        .into_iter()
        .zip(versions)
        .map(|((name, image, windows), version)| ArmRecord {
            name,
            spec: image,
            version,
            windows,
        })
        .collect();

    Ok(CompareRecord {
        timestamp: report::now_rfc3339(),
        git_sha,
        rom_sha256,
        k: args.k,
        hwm: args.hwm,
        repeats: args.repeats,
        batches: args.batches,
        note: args.note.clone(),
        arms,
    })
}
