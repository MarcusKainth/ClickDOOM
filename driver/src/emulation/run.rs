//! The resumable batch-loop runner for a multi-hour run against the real
//! ROM, through every `FRAME_COMMIT` to a target icount in one invocation.
//!
//! Resumable with no snapshot file of its own: every `commit` flush is
//! idempotent, keyed on the latest `batch_id`, so "resume" is just the
//! `SELECT max(batch_id), max(icount) FROM cpu_state FINAL` progress query,
//! read once at startup. Loops [`clickdoom_executor::fold::batch`] then the
//! `commit` flushes, called exactly as those modules define them.
//!
//! Each batch is passed `min(K, next_boundary - current_icount)` rather
//! than a constant K, so a batch lands exactly on the next
//! `RAM_HASH_INTERVAL` boundary rather than almost always missing it: SPEC's
//! checkpoint intervals don't divide evenly into any fixed K, and a check
//! nothing ever lands on is indistinguishable from one that passes.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration; // purity-ok: the progress line's reporting interval, off every computation path

use clickdoom_executor::commit;
use clickdoom_executor::config::BATCH_COMMIT_RETENTION_N;
use clickdoom_executor::fold::{self, BatchArgs};
use clickdoom_spec::{Manifest, RAM_BASE, RAM_HASH_INTERVAL};

use crate::checkpoint::checkpoint_sql;
use crate::client::{ConnArgs, Error};
use crate::emulation::preflight;
use crate::emulation::rom::RAM_WORDS_DEFAULT;
use crate::frames;
use crate::render;
use crate::stats::{Counters, Monotonic, StatsLine};

/// How often the progress line is printed. A batch at the default K takes
/// longer than this, so in practice one line follows each batch.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Preflight(#[from] preflight::GateError),
    #[error(transparent)]
    Bootstrap(#[from] crate::emulation::bootstrap::SeedError),
    #[error(transparent)]
    Manifest(#[from] clickdoom_spec::manifest::ManifestError),
    #[error(transparent)]
    Db(#[from] Error),
    #[error("reading {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Frame(#[from] crate::frames::Error),
    #[error("no reference trace line for icount={0} in {1}: trace/run icount cadence disagree")]
    NoTraceLine(u64, std::path::PathBuf),
    #[error(
        "checkpoint mismatch at icount={icount}: expected (icount/pc/reghash/ramhash/fbhash) {expected}, actual {actual}"
    )]
    CheckpointMismatch {
        icount: u64,
        expected: String,
        actual: String,
    },
    #[error("fatal halt ({reason}) at icount={icount}, short of target icount={target}")]
    FatalHalt {
        reason: String,
        icount: u64,
        target: u64,
    },
}

pub struct Args<'a> {
    pub bin: &'a Path,
    pub manifest_path: &'a Path,
    pub k: u32,
    pub hwm: u32,
    pub trace_path: &'a Path,
    pub target_icount: u64,
    pub stop_at_frame: Option<u32>,
    /// Where to write each committed frame as a PPM. `None` writes none.
    pub frame_dir: Option<&'a Path>,
}

pub enum Stop {
    Interrupted,
    ReachedTarget,
    HaltedAtOrPastTarget { reason: String },
}

pub struct Outcome {
    pub stop: Stop,
    pub final_batch_id: u64,
    pub final_icount: u64,
    pub frames_observed: u32,
}

/// Reads `[icount, pc_hex, reghash_hex, ramhash_hex, fbhash_hex]` from a
/// reference trace TSV, returning the row whose icount matches.
fn trace_line_for(trace_path: &Path, icount: u64) -> Result<String, RunError> {
    let text = std::fs::read_to_string(trace_path).map_err(|source| RunError::Read {
        path: trace_path.to_owned(),
        source,
    })?;
    text.lines()
        .find(|line| line.split('\t').next() == Some(&icount.to_string()))
        .map(str::to_string)
        .ok_or_else(|| RunError::NoTraceLine(icount, trace_path.to_owned()))
}

/// Runs the batch loop until the target icount, `--stop-at-frame`, a fatal
/// halt, or an interrupt (SIGINT/SIGTERM) stops it. A batch that halts or
/// hits the write-log high-water mark short of `k` is not treated as an
/// error here: the loop always re-reads the real retired icount rather
/// than assuming a batch reached what it was asked for.
pub async fn run(conn: &ConnArgs, args: &Args<'_>) -> Result<Outcome, RunError> {
    if let Some(dir) = args.frame_dir {
        frames::prepare(dir)?;
    }

    let db = conn.connect();

    preflight::check(&db, conn, args.bin, args.manifest_path, args.k, args.hwm).await?;

    crate::emulation::bootstrap::seed(&db, &crate::emulation::bootstrap::RESET_REGS).await?;
    db.run(&commit::cpu_state_flush_sql(&conn.database)).await?;

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

    let (resume_batch, resume_icount): (u64, u64) = db
        .fetch_one("SELECT max(batch_id), max(icount) FROM cpu_state FINAL")
        .await?;
    eprintln!("# resuming from batch_id={resume_batch} icount={resume_icount}");

    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let flag = interrupted.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                flag.store(true, Ordering::SeqCst);
            }
        });
    }

    let mut icount = resume_icount;
    let mut batch_id = resume_batch;
    let mut halted_reason = String::new();
    let mut reached_target = false;
    let mut frames_observed = 0u32;
    let mut batches_run = 0u64;
    let ram_hash_interval = RAM_HASH_INTERVAL;
    // Counted from the resume point, so the reported rates are this
    // process's and not the whole run's history.
    let mut stats = StatsLine::start(Monotonic::new(), STATS_INTERVAL);
    let progress = |icount: u64, batches: u64, frames: u32| Counters {
        instructions: icount - resume_icount,
        batches,
        frames: u64::from(frames),
    };

    while !interrupted.load(Ordering::SeqCst) {
        if icount >= args.target_icount {
            reached_target = true;
            break;
        }

        let next_boundary = ((icount / ram_hash_interval) + 1) * ram_hash_interval;
        let stop_at = next_boundary.min(args.target_icount);
        let remaining = stop_at - icount;
        let step_k = if remaining > 0 && remaining < args.k as u64 {
            remaining as u32
        } else {
            args.k
        };

        let batch_args = BatchArgs {
            db: &conn.database,
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
        db.run(&commit::ram_flush_sql(&conn.database)).await?;
        // fbpal_flush_sql returns two statements (framebuffer, then
        // palette) in one string; the HTTP interface takes one statement
        // per request.
        for statement in crate::sql::split_statements(&commit::fbpal_flush_sql(&conn.database)) {
            db.run(statement).await?;
        }
        db.run(&commit::console_out_flush_sql(&conn.database))
            .await?;
        db.run(&commit::cpu_state_flush_sql(&conn.database)).await?;
        db.run(&commit::retention_sql(
            &conn.database,
            BATCH_COMMIT_RETENTION_N,
        ))
        .await?;

        let (new_batch_id, new_icount, pc, halted, halt_reason): (u64, u64, u32, u8, String) = db
            .fetch_one("SELECT batch_id, icount, pc, halted, halt_reason FROM cpu_state ORDER BY batch_id DESC LIMIT 1")
            .await?;
        let (has_frame, frame_no): (u8, u32) = db
            .fetch_one(
                "SELECT has_frame, frame_no FROM batch_commit ORDER BY batch_id DESC LIMIT 1",
            )
            .await?;
        batch_id = new_batch_id;
        icount = new_icount;
        batches_run += 1;
        eprintln!(
            "# batch_id={batch_id} icount={icount} pc={pc:#010x} halted={halted} halt_reason={halt_reason} has_frame={has_frame} frame_no={frame_no}"
        );
        if let Some(line) = stats.tick(progress(icount, batches_run, frames_observed)) {
            eprintln!("{line}");
        }

        if icount.is_multiple_of(ram_hash_interval) && icount > 0 {
            let actual: String = db.fetch_one(&checkpoint_sql(&conn.database)).await?;
            let expected = trace_line_for(args.trace_path, icount)?;
            if actual != expected {
                return Err(RunError::CheckpointMismatch {
                    icount,
                    expected,
                    actual,
                });
            }
            eprintln!("# checkpoint OK at icount={icount}");
        }

        if halted == 1 {
            halted_reason = halt_reason;
            break;
        }
        if has_frame == 1 {
            frames_observed += 1;
            db.run(&render::frame_readout_sql(&conn.database)).await?;
            let fb_hash: String = db
                .fetch_one(&render::frame_readout_fb_hash_sql(&conn.database))
                .await?;
            eprintln!(
                "# FRAME_COMMIT observed: frame_no={frame_no} icount={icount} fb_hash={fb_hash} (frames_observed={frames_observed})"
            );
            if let Some(dir) = args.frame_dir {
                let path = frames::write_committed(&db, &conn.database, dir, frame_no).await?;
                eprintln!("# wrote {}", path.display());
            }
            if let Some(stop_at_frame) = args.stop_at_frame
                && frame_no >= stop_at_frame
            {
                reached_target = true;
                break;
            }
        }
    }

    // Before the stop is classified, so a fatal halt reports its totals too.
    eprintln!(
        "{}",
        stats.finish(progress(icount, batches_run, frames_observed))
    );

    let stop = if interrupted.load(Ordering::SeqCst) {
        eprintln!(
            "# stopped: interrupted, current batch's flush already committed -- safe to resume"
        );
        Stop::Interrupted
    } else if !halted_reason.is_empty() {
        eprintln!("# stopped: halted, reason={halted_reason}, icount={icount}");
        if icount < args.target_icount {
            return Err(RunError::FatalHalt {
                reason: halted_reason,
                icount,
                target: args.target_icount,
            });
        }
        Stop::HaltedAtOrPastTarget {
            reason: halted_reason,
        }
    } else {
        debug_assert!(reached_target);
        eprintln!(
            "# stopped cleanly: icount={icount} target={} frames_observed={frames_observed}",
            args.target_icount
        );
        Stop::ReachedTarget
    };

    Ok(Outcome {
        stop,
        final_batch_id: batch_id,
        final_icount: icount,
        frames_observed,
    })
}
