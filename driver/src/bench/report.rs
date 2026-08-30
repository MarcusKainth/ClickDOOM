//! Git-friendly historic records for a bench run: one JSON line appended
//! per invocation, and a Markdown renderer over one or more records for
//! pasting into a PR comment.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::canonical::Report as CanonicalReport;

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("creating {0}: {1}")]
    CreateDir(std::path::PathBuf, std::io::Error),
    #[error("opening {0}: {1}")]
    Open(std::path::PathBuf, std::io::Error),
    #[error("writing {0}: {1}")]
    Write(std::path::PathBuf, std::io::Error),
    #[error("reading {0}: {1}")]
    Read(std::path::PathBuf, std::io::Error),
    #[error("parsing a JSONL line in {0}: {1}")]
    Parse(std::path::PathBuf, serde_json::Error),
    #[error("{0} has no records")]
    Empty(std::path::PathBuf),
    #[error("{0} has {1} record(s), no record at index {2}")]
    NoSuchRun(std::path::PathBuf, usize, usize),
}

#[derive(Serialize, Deserialize, Clone)]
pub struct WindowRecord {
    pub label: String,
    pub k: u32,
    pub hwm: u32,
    pub fold_retired: u64,
    pub fold_seconds: f64,
    pub e2e_retired: u64,
    pub e2e_seconds: f64,
}

impl WindowRecord {
    pub fn fold_instr_per_sec(&self) -> f64 {
        self.fold_retired as f64 / self.fold_seconds
    }
    pub fn e2e_instr_per_sec(&self) -> f64 {
        self.e2e_retired as f64 / self.e2e_seconds
    }
}

/// One `bench canonical` invocation, one ClickHouse version.
#[derive(Serialize, Deserialize, Clone)]
pub struct CanonicalRecord {
    pub timestamp: String,
    pub git_sha: String,
    pub rom_sha256: String,
    pub k: u32,
    pub hwm: u32,
    pub batches: u32,
    pub note: Option<String>,
    pub windows: Vec<WindowRecord>,
}

impl From<&CanonicalReport> for CanonicalRecord {
    fn from(report: &CanonicalReport) -> Self {
        CanonicalRecord {
            timestamp: now_rfc3339(),
            git_sha: report.git_sha.clone(),
            rom_sha256: report.rom_sha256.clone(),
            k: report.k,
            hwm: report.hwm,
            batches: report.batches,
            note: None,
            windows: report
                .windows
                .iter()
                .map(|w| WindowRecord {
                    label: w.label.clone(),
                    k: w.k,
                    hwm: w.hwm,
                    fold_retired: w.fold.retired,
                    fold_seconds: w.fold.seconds,
                    e2e_retired: w.e2e.retired,
                    e2e_seconds: w.e2e.seconds,
                })
                .collect(),
        }
    }
}

/// One arm of a `bench compare-versions` run: a name, the server version
/// that answered, and one `WindowRecord` per (repeat, window).
#[derive(Serialize, Deserialize, Clone)]
pub struct ArmRecord {
    pub name: String,
    pub spec: String,
    pub version: String,
    pub windows: Vec<WindowRecord>,
}

/// One `bench compare-versions` invocation, comparing every arm.
#[derive(Serialize, Deserialize, Clone)]
pub struct CompareRecord {
    pub timestamp: String,
    pub git_sha: String,
    pub rom_sha256: String,
    pub k: u32,
    pub hwm: u32,
    pub repeats: u32,
    pub batches: u32,
    pub note: Option<String>,
    pub arms: Vec<ArmRecord>,
}

/// purity-ok: a wall-clock timestamp on a benchmark record, host-side
/// reporting metadata, never a value the emulated machine computes with.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true) // purity-ok: a record's own timestamp field, not used in any query
}

fn append_jsonl<T: Serialize>(path: &Path, record: &T) -> Result<(), ReportError> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ReportError::CreateDir(parent.to_owned(), e))?;
    }
    let line = serde_json::to_string(record).expect("bench records serialize");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| ReportError::Open(path.to_owned(), e))?;
    writeln!(file, "{line}").map_err(|e| ReportError::Write(path.to_owned(), e))?;
    Ok(())
}

pub fn append_canonical(path: &Path, record: &CanonicalRecord) -> Result<(), ReportError> {
    append_jsonl(path, record)
}

pub fn append_compare(path: &Path, record: &CompareRecord) -> Result<(), ReportError> {
    append_jsonl(path, record)
}

fn read_jsonl<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<Vec<T>, ReportError> {
    let text = std::fs::read_to_string(path).map_err(|e| ReportError::Read(path.to_owned(), e))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| ReportError::Parse(path.to_owned(), e)))
        .collect()
}

/// Which record in a JSONL history file to render.
pub enum Selector {
    Latest,
    Index(usize),
}

pub fn select_canonical(path: &Path, which: Selector) -> Result<CanonicalRecord, ReportError> {
    let records: Vec<CanonicalRecord> = read_jsonl(path)?;
    pick(records, path, which)
}

pub fn select_compare(path: &Path, which: Selector) -> Result<CompareRecord, ReportError> {
    let records: Vec<CompareRecord> = read_jsonl(path)?;
    pick(records, path, which)
}

fn pick<T>(mut records: Vec<T>, path: &Path, which: Selector) -> Result<T, ReportError> {
    if records.is_empty() {
        return Err(ReportError::Empty(path.to_owned()));
    }
    match which {
        Selector::Latest => Ok(records.pop().expect("checked non-empty")),
        Selector::Index(i) => {
            if i >= records.len() {
                return Err(ReportError::NoSuchRun(path.to_owned(), records.len(), i));
            }
            Ok(records.swap_remove(i))
        }
    }
}

fn window_table(windows: &[WindowRecord]) -> String {
    let mut out = String::from(
        "| window | mode | k | hwm | retired | instr/s |\n|---|---|---|---|---|---|\n",
    );
    for w in windows {
        out.push_str(&format!(
            "| {} | fold-alone | {} | {} | {} | {:.1} |\n",
            w.label,
            w.k,
            w.hwm,
            w.fold_retired,
            w.fold_instr_per_sec()
        ));
        out.push_str(&format!(
            "| {} | e2e | {} | {} | {} | {:.1} |\n",
            w.label,
            w.k,
            w.hwm,
            w.e2e_retired,
            w.e2e_instr_per_sec()
        ));
    }
    out
}

pub fn render_canonical(record: &CanonicalRecord) -> String {
    let mut out = format!(
        "`bench canonical`, {}, K={}, HWM={}, {} batch(es), ROM {}, {}\n\n",
        record.timestamp,
        record.k,
        record.hwm,
        record.batches,
        &record.rom_sha256[..12.min(record.rom_sha256.len())],
        &record.git_sha[..12.min(record.git_sha.len())]
    );
    if let Some(note) = &record.note {
        out.push_str(&format!("Machine: {note}\n\n"));
    } else {
        out.push_str("Machine: TODO -- how quiet was it?\n\n");
    }
    out.push_str(&window_table(&record.windows));
    out
}

pub fn render_compare(record: &CompareRecord) -> String {
    let mut out = format!(
        "`bench compare-versions`, {}, K={}, HWM={}, {} repeat(s) of {} batch(es), ROM {}, {}\n\n",
        record.timestamp,
        record.k,
        record.hwm,
        record.repeats,
        record.batches,
        &record.rom_sha256[..12.min(record.rom_sha256.len())],
        &record.git_sha[..12.min(record.git_sha.len())]
    );
    if let Some(note) = &record.note {
        out.push_str(&format!("Machine: {note}\n\n"));
    } else {
        out.push_str("Machine: TODO -- how quiet was it?\n\n");
    }
    for arm in &record.arms {
        out.push_str(&format!(
            "### {} ({}, {})\n\n",
            arm.name, arm.spec, arm.version
        ));
        out.push_str(&window_table(&arm.windows));
        out.push('\n');
    }

    if record.arms.len() >= 2 {
        out.push_str("### Speedup, first arm as baseline\n\n");
        out.push_str("| window | mode | arm | vs baseline |\n|---|---|---|---|\n");
        let baseline = &record.arms[0];
        for arm in &record.arms[1..] {
            for (base_w, w) in baseline.windows.iter().zip(arm.windows.iter()) {
                let fold_ratio = w.fold_instr_per_sec() / base_w.fold_instr_per_sec();
                let e2e_ratio = w.e2e_instr_per_sec() / base_w.e2e_instr_per_sec();
                out.push_str(&format!(
                    "| {} | fold-alone | {} | {:.2}x |\n",
                    w.label, arm.name, fold_ratio
                ));
                out.push_str(&format!(
                    "| {} | e2e | {} | {:.2}x |\n",
                    w.label, arm.name, e2e_ratio
                ));
            }
        }
    }
    out
}
