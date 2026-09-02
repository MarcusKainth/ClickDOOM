//! The full-cadence differential runner against `refemu`.
//!
//! `run`'s checkpoint comparison only ever lands on `RAM_HASH_INTERVAL`
//! boundaries, 1 in 256 of the checkpoint rows both engines emit. This
//! clamps every batch to `CHECKPOINT_INTERVAL` instead, so no register or
//! control-flow checkpoint is ever skipped, and asserts the number of
//! checkpoints actually compared against a count cross-checked against
//! `refemu`'s own trace file content: a loop that silently compares nothing
//! must fail loudly here, not read as identical to one that compared
//! everything.
//!
//! Register and control-flow divergence (icount/pc/reghash) is caught at
//! every `CHECKPOINT_INTERVAL` landing. Memory and framebuffer divergence
//! (ramhash/fbhash) are only ever checked at the 256x rarer
//! `RAM_HASH_INTERVAL` landing, exactly as the checkpoint format defines it:
//! a register file can agree bit for bit across hundreds of thousands of
//! instructions while an unread memory word sits wrong the entire time.
//!
//! Every run provisions a throwaway database and drives `refemu` for the
//! same instruction count, both from icount 0: neither engine resumes from
//! prior state, so a diff run's result never depends on the driver's own
//! production state. `refemu` runs as a separate process and is read back
//! through its own halt-report and trace-file formats, not linked in: it is
//! the independent oracle here, not a library this crate happens to use.

use std::path::{Path, PathBuf};
use std::process::Command;

use clickdoom_executor::commit;
use clickdoom_executor::config::BATCH_COMMIT_RETENTION_N;
use clickdoom_executor::fold::{self, BatchArgs};
use clickdoom_spec::{CHECKPOINT_INTERVAL, Manifest, RAM_BASE, RAM_HASH_INTERVAL, sha256_hex};
use refemu::cli::report::RunReport;

use crate::bench::canonical::{CanonicalError, create_and_load_database, db_at};
use crate::checkpoint::{checkpoint_sql, reg_checkpoint_sql};
use crate::client::{ConnArgs, Error};
use crate::emulation::preflight::PINNED_HASH;
use crate::emulation::rom::RAM_WORDS_DEFAULT;
use crate::sql::split_statements;

#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    #[error("{0}: sha256 ({1}) != rom/PINNED_HASH ({2})")]
    RomHash(PathBuf, String, String),
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    HaltReportParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Manifest(#[from] clickdoom_spec::manifest::ManifestError),
    #[error("running {0}: {1}")]
    Spawn(String, std::io::Error),
    #[error("{0} exited with {1}, refemu's own image or file arguments are likely wrong")]
    RefemuFailed(String, std::process::ExitStatus),
    #[error(
        "no refemu trace line for icount={0}: refemu and sqlcpu disagree about where a checkpoint falls, which is structurally impossible since both use CHECKPOINT_INTERVAL={1}"
    )]
    NoTraceLine(u64, u64),
    #[error(
        "checkpoint mismatch at icount={icount}: expected (refemu) {expected}, actual (sqlcpu) {actual}"
    )]
    CheckpointMismatch {
        icount: u64,
        expected: String,
        actual: String,
    },
    #[error(
        "sqlcpu retired past icount={icount} but refemu halted earlier, at icount={refemu_halt_icount} (reason={refemu_halt_reason})"
    )]
    SqlcpuOutranRefemuHalt {
        icount: u64,
        refemu_halt_icount: u64,
        refemu_halt_reason: String,
    },
    #[error(
        "sqlcpu halted (reason={sqlcpu_halt_reason} icount={icount}) but refemu did not halt in [0,{target})"
    )]
    SqlcpuHaltedAlone {
        sqlcpu_halt_reason: String,
        icount: u64,
        target: u64,
    },
    #[error(
        "refemu halted (reason={refemu_halt_reason} icount={refemu_halt_icount}) but sqlcpu did not"
    )]
    RefemuHaltedAlone {
        refemu_halt_reason: String,
        refemu_halt_icount: u64,
    },
    #[error(
        "halt shape mismatch: refemu(reason={refemu_halt_reason} icount={refemu_halt_icount}) vs sqlcpu(reason={sqlcpu_halt_reason} icount={sqlcpu_icount})"
    )]
    HaltShapeMismatch {
        refemu_halt_reason: String,
        refemu_halt_icount: u64,
        sqlcpu_halt_reason: String,
        sqlcpu_icount: u64,
    },
    #[error(
        "refemu's own trace has {actual} {kind} rows <= icount={final_icount}, expected {expected} by the checkpoint format's own cadence arithmetic -- the oracle disagrees with the format it defines; investigate refemu before trusting anything else this run reported"
    )]
    OracleTraceShortfall {
        kind: &'static str,
        actual: u64,
        expected: u64,
        final_icount: u64,
    },
    #[error(
        "compared {compared} of {expected} expected {kind} rows in [0,{final_icount}] -- a runner that silently skips rows is indistinguishable from one that checked them all unless this assertion fires"
    )]
    CountShortfall {
        kind: &'static str,
        compared: u64,
        expected: u64,
        final_icount: u64,
    },
    #[error(transparent)]
    Db(#[from] Error),
    #[error(transparent)]
    Bootstrap(#[from] crate::emulation::bootstrap::SeedError),
    #[error(transparent)]
    Provision(#[from] CanonicalError),
}

pub struct Args<'a> {
    pub bin: &'a Path,
    pub manifest_path: &'a Path,
    pub hwm: u32,
    pub database: String,
    /// Leave the database in place on exit, for inspecting a caught
    /// divergence.
    pub keep_db: bool,
    pub refemu_bin: PathBuf,
    pub target_icount: u64,
}

/// What a diff run found, once every gate below has already passed.
pub struct Outcome {
    pub rom_sha256: String,
    pub clickhouse_version: String,
    pub requested_instructions: u64,
    pub final_icount: u64,
    pub batches_run: u32,
    pub checkpoints_compared: u64,
    pub checkpoints_expected: u64,
    pub ram_hash_checkpoints_compared: u64,
    pub ram_hash_checkpoints_expected: u64,
    pub sqlcpu_halted: bool,
}

fn trace_line_for(trace_path: &Path, icount: u64) -> Result<String, DiffError> {
    let text = std::fs::read_to_string(trace_path).map_err(|source| DiffError::Read {
        path: trace_path.to_owned(),
        source,
    })?;
    text.lines()
        .find(|line| line.split('\t').next() == Some(&icount.to_string()))
        .map(str::to_string)
        .ok_or(DiffError::NoTraceLine(icount, CHECKPOINT_INTERVAL))
}

/// `(rows with icount <= final_icount, of those, the 5-field RAM_HASH_INTERVAL rows)`.
fn trace_rows_in_range(trace_path: &Path, final_icount: u64) -> Result<(u64, u64), DiffError> {
    let text = std::fs::read_to_string(trace_path).map_err(|source| DiffError::Read {
        path: trace_path.to_owned(),
        source,
    })?;
    let mut rows = 0u64;
    let mut ram_hash_rows = 0u64;
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let Some(icount) = fields.first().and_then(|s| s.parse::<u64>().ok()) else {
            continue;
        };
        if icount > final_icount {
            continue;
        }
        rows += 1;
        if fields.len() == 5 {
            ram_hash_rows += 1;
        }
    }
    Ok((rows, ram_hash_rows))
}

/// Runs `refemu trace` for `target_icount` instructions, writing the
/// checkpoint trace to `trace_path` and the halt report to
/// `halt_report_path`. Exit code 0 (reached the budget as a stop) and 4
/// (the ordinary "ran out of budget" exit, since no `--stop-at` is given
/// here) are both expected outcomes; 3 (halted) is as well, since a halt
/// before the budget is a real, comparable outcome, not a refemu failure.
fn generate_reference_trace(
    refemu_bin: &Path,
    bin: &Path,
    manifest_path: &Path,
    target_icount: u64,
    trace_path: &Path,
    halt_report_path: &Path,
) -> Result<(), DiffError> {
    let status = Command::new(refemu_bin)
        .arg("trace")
        .arg(bin)
        .arg("--manifest")
        .arg(manifest_path)
        .arg("--max-instructions")
        .arg(target_icount.to_string())
        .arg("--out")
        .arg(trace_path)
        .arg("--halt-report")
        .arg(halt_report_path)
        .status()
        .map_err(|e| DiffError::Spawn(refemu_bin.display().to_string(), e))?;
    match status.code() {
        Some(0 | 3 | 4) => Ok(()),
        _ => Err(DiffError::RefemuFailed(
            refemu_bin.display().to_string(),
            status,
        )),
    }
}

struct RefemuHalt {
    halted: bool,
    reason: String,
    icount: u64,
}

fn read_halt_report(path: &Path) -> Result<RefemuHalt, DiffError> {
    let text = std::fs::read_to_string(path).map_err(|source| DiffError::Read {
        path: path.to_owned(),
        source,
    })?;
    let report: RunReport =
        serde_json::from_str(&text).map_err(|source| DiffError::HaltReportParse {
            path: path.to_owned(),
            source,
        })?;
    Ok(match report.halt {
        Some(halt) => RefemuHalt {
            halted: true,
            reason: halt.reason.to_string(),
            icount: report.icount,
        },
        None => RefemuHalt {
            halted: false,
            reason: String::new(),
            icount: 0,
        },
    })
}

/// Runs the whole differential comparison. The ephemeral database and the
/// two temporary refemu output files are always cleaned up on the way out,
/// success or failure, unless `args.keep_db` asks to leave the database for
/// inspection.
pub async fn run(conn: &ConnArgs, args: &Args<'_>) -> Result<Outcome, DiffError> {
    let trace_path =
        std::env::temp_dir().join(format!("clickdoom-diff-trace-{}.tsv", std::process::id()));
    let halt_report_path =
        std::env::temp_dir().join(format!("clickdoom-diff-halt-{}.json", std::process::id()));

    let result = run_inner(conn, args, &trace_path, &halt_report_path).await;

    let _ = std::fs::remove_file(&trace_path);
    let _ = std::fs::remove_file(&halt_report_path);

    if args.keep_db {
        return result;
    }
    let admin = db_at(conn, "default");
    let dropped = admin
        .run(&format!("DROP DATABASE IF EXISTS {}", args.database))
        .await;
    let outcome = result?;
    dropped?;
    Ok(outcome)
}

async fn run_inner(
    conn: &ConnArgs,
    args: &Args<'_>,
    trace_path: &Path,
    halt_report_path: &Path,
) -> Result<Outcome, DiffError> {
    let blob = std::fs::read(args.bin).map_err(|source| DiffError::Read {
        path: args.bin.to_owned(),
        source,
    })?;
    let rom_sha256 = sha256_hex(&blob);
    let pinned = PINNED_HASH.trim();
    if rom_sha256 != pinned {
        return Err(DiffError::RomHash(
            args.bin.to_owned(),
            rom_sha256,
            pinned.to_string(),
        ));
    }

    let manifest = Manifest::read(args.manifest_path)?;
    let text_start = manifest.text_start.unwrap_or(RAM_BASE);
    let text_end = manifest.text_end.unwrap_or(RAM_BASE);
    let load_addr = manifest.load_addr.unwrap_or(RAM_BASE);
    let text_start_word = text_start / 4;
    let text_end_word = text_end / 4;
    let ram_base_word = load_addr / 4;
    let text_start_widx = text_start_word - ram_base_word;
    let text_end_widx = text_end_word - ram_base_word;
    let decn = text_end_word - text_start_word;
    let ram_words = RAM_WORDS_DEFAULT;

    eprintln!(
        "# --- provisioning ephemeral database '{}' (fresh, icount 0) ---",
        args.database
    );
    create_and_load_database(
        &db_at(conn, "default"),
        conn,
        &args.database,
        args.bin,
        args.manifest_path,
        text_start_word,
        text_end_word,
    )
    .await?;
    let db = db_at(conn, &args.database);
    crate::emulation::bootstrap::seed(&db, &crate::emulation::bootstrap::RESET_REGS).await?;
    db.run(&commit::cpu_state_flush_sql(&args.database)).await?;
    let clickhouse_version: String = db.fetch_one("SELECT version()").await?;
    eprintln!("  provisioned: decoded rows={decn}, ClickHouse {clickhouse_version}");

    eprintln!(
        "# --- generating refemu's checkpoint trace for {} instructions ---",
        args.target_icount
    );
    generate_reference_trace(
        &args.refemu_bin,
        args.bin,
        args.manifest_path,
        args.target_icount,
        trace_path,
        halt_report_path,
    )?;
    let refemu_halt = read_halt_report(halt_report_path)?;
    if refemu_halt.halted {
        eprintln!(
            "  refemu halted: {} at icount={}",
            refemu_halt.reason, refemu_halt.icount
        );
    }

    eprintln!("# --- differential loop: sqlcpu batches clamped to CHECKPOINT_INTERVAL ---");
    let mut icount = 0u64;
    let mut checkpoints_compared = 0u64;
    let mut ram_hash_checkpoints_compared = 0u64;
    let mut sqlcpu_halted = false;
    let mut sqlcpu_halt_reason = String::new();
    let mut batches_run = 0u32;

    while icount < args.target_icount && !sqlcpu_halted {
        let next_boundary = ((icount / CHECKPOINT_INTERVAL) + 1) * CHECKPOINT_INTERVAL;
        let stop_at = next_boundary.min(args.target_icount);
        let step_k = (stop_at - icount) as u32;

        let batch_args = BatchArgs {
            db: &args.database,
            ..Default::default()
        };
        let batch_sql = fold::batch(
            step_k,
            text_start_widx,
            text_end_widx,
            decn,
            ram_words,
            args.hwm,
            &batch_args,
        );
        db.run(&batch_sql).await?;
        db.run(&commit::ram_flush_sql(&args.database)).await?;
        // fbpal_flush_sql returns two statements (framebuffer, then
        // palette) in one string; the HTTP interface takes one statement
        // per request.
        for statement in split_statements(&commit::fbpal_flush_sql(&args.database)) {
            db.run(statement).await?;
        }
        db.run(&commit::console_out_flush_sql(&args.database))
            .await?;
        db.run(&commit::cpu_state_flush_sql(&args.database)).await?;
        db.run(&commit::retention_sql(
            &args.database,
            BATCH_COMMIT_RETENTION_N,
        ))
        .await?;
        batches_run += 1;

        // Never trust step_k as what actually retired: a batch can stop
        // early on halt, FRAME_COMMIT, or the write-log high-water mark.
        let (new_icount, _pc, halted, halt_reason): (u64, u32, u8, String) = db
            .fetch_one(
                "SELECT icount, pc, halted, halt_reason FROM cpu_state ORDER BY batch_id DESC LIMIT 1",
            )
            .await?;
        icount = new_icount;

        if icount > 0 && icount.is_multiple_of(CHECKPOINT_INTERVAL) {
            if refemu_halt.halted && icount > refemu_halt.icount {
                return Err(DiffError::SqlcpuOutranRefemuHalt {
                    icount,
                    refemu_halt_icount: refemu_halt.icount,
                    refemu_halt_reason: refemu_halt.reason.clone(),
                });
            }

            let at_ram_hash = icount.is_multiple_of(RAM_HASH_INTERVAL);
            let actual: String = if at_ram_hash {
                db.fetch_one(&checkpoint_sql(&args.database)).await?
            } else {
                db.fetch_one(&reg_checkpoint_sql(&args.database)).await?
            };
            let expected = trace_line_for(trace_path, icount)?;
            if actual != expected {
                return Err(DiffError::CheckpointMismatch {
                    icount,
                    expected,
                    actual,
                });
            }
            checkpoints_compared += 1;
            if at_ram_hash {
                ram_hash_checkpoints_compared += 1;
            }
        }

        if halted == 1 {
            sqlcpu_halted = true;
            sqlcpu_halt_reason = halt_reason;
        }
    }

    eprintln!("# --- halt-shape comparison ---");
    match (sqlcpu_halted, refemu_halt.halted) {
        (true, false) => {
            return Err(DiffError::SqlcpuHaltedAlone {
                sqlcpu_halt_reason,
                icount,
                target: args.target_icount,
            });
        }
        (false, true) if icount >= refemu_halt.icount => {
            return Err(DiffError::RefemuHaltedAlone {
                refemu_halt_reason: refemu_halt.reason,
                refemu_halt_icount: refemu_halt.icount,
            });
        }
        (true, true) => {
            if icount != refemu_halt.icount || sqlcpu_halt_reason != refemu_halt.reason {
                return Err(DiffError::HaltShapeMismatch {
                    refemu_halt_reason: refemu_halt.reason,
                    refemu_halt_icount: refemu_halt.icount,
                    sqlcpu_halt_reason,
                    sqlcpu_icount: icount,
                });
            }
            eprintln!(
                "  both engines halted identically: reason={sqlcpu_halt_reason} icount={icount}"
            );
        }
        _ => eprintln!(
            "  neither engine halted in [0,{}) -- nothing to compare here",
            args.target_icount
        ),
    }

    eprintln!("# --- comparison-count assertion ---");
    let final_icount = icount;
    let checkpoints_expected = final_icount / CHECKPOINT_INTERVAL;
    let ram_hash_checkpoints_expected = final_icount / RAM_HASH_INTERVAL;
    let (refemu_rows_in_range, refemu_ram_hash_rows_in_range) =
        trace_rows_in_range(trace_path, final_icount)?;

    if refemu_rows_in_range != checkpoints_expected {
        return Err(DiffError::OracleTraceShortfall {
            kind: "CHECKPOINT_INTERVAL",
            actual: refemu_rows_in_range,
            expected: checkpoints_expected,
            final_icount,
        });
    }
    if checkpoints_compared != checkpoints_expected {
        return Err(DiffError::CountShortfall {
            kind: "CHECKPOINT_INTERVAL",
            compared: checkpoints_compared,
            expected: checkpoints_expected,
            final_icount,
        });
    }
    if refemu_ram_hash_rows_in_range != ram_hash_checkpoints_expected {
        return Err(DiffError::OracleTraceShortfall {
            kind: "RAM_HASH_INTERVAL",
            actual: refemu_ram_hash_rows_in_range,
            expected: ram_hash_checkpoints_expected,
            final_icount,
        });
    }
    if ram_hash_checkpoints_compared != ram_hash_checkpoints_expected {
        return Err(DiffError::CountShortfall {
            kind: "RAM_HASH_INTERVAL",
            compared: ram_hash_checkpoints_compared,
            expected: ram_hash_checkpoints_expected,
            final_icount,
        });
    }

    Ok(Outcome {
        rom_sha256,
        clickhouse_version,
        requested_instructions: args.target_icount,
        final_icount,
        batches_run,
        checkpoints_compared,
        checkpoints_expected,
        ram_hash_checkpoints_compared,
        ram_hash_checkpoints_expected,
        sqlcpu_halted,
    })
}
