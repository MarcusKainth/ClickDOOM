//! The resumable batch-loop runner for a multi-hour run against the real
//! ROM, through every `FRAME_COMMIT` to a target icount in one invocation.
//!
//! Resumable with no snapshot file of its own: `batch_commit`'s latest row
//! says where the run has got to, and every `commit` flush is idempotent,
//! so "resume" is that one query plus a redo of that row's derivations.
//! Loops [`clickdoom_executor::fold::batch`] then the `commit` flushes,
//! called exactly as those modules define them.
//!
//! Every flush names the batch this loop just ran, and the loop refuses to
//! flush a batch id other than the one it expected to commit. A second
//! runner against the same database moves `batch_commit`'s maximum, and a
//! flush that read the maximum instead would derive that runner's batch and
//! drop this one's write-log with no error anywhere.
//!
//! Each batch is passed `min(K, next_boundary - current_icount)` rather
//! than a constant K, so a batch lands exactly on the next
//! `RAM_HASH_INTERVAL` boundary rather than almost always missing it: SPEC's
//! checkpoint intervals don't divide evenly into any fixed K, and a check
//! nothing ever lands on is indistinguishable from one that passes.
//!
//! The register cadence is 256x finer than that and a batch cannot land on
//! every one of its boundaries, so the fold records `(icount, pc, regs)` at
//! each boundary it crosses and commits them with the batch. Both cadences
//! are compared against the reference trace after every batch, and the run
//! stops on the first line that differs. A register checkpoint carries no
//! `ramhash`, so it catches register and control-flow divergence only.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration; // purity-ok: the progress line's reporting interval, off every computation path

use clickdoom_executor::commit;
use clickdoom_executor::config::BATCH_COMMIT_RETENTION_N;
use clickdoom_executor::fold::{self, BatchArgs};
use clickdoom_executor::word::WordAddr;
use clickdoom_spec::{CHECKPOINT_INTERVAL, Manifest, RAM_BASE, RAM_HASH_INTERVAL};

use crate::checkpoint::{batch_checkpoints_sql, checkpoint_sql, reg_checkpoint_sql};
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
    Rebase(#[from] clickdoom_executor::word::BelowBase),
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
    #[error("checkpoint mismatch at icount={icount}: expected (trace) {expected}, actual {actual}")]
    CheckpointMismatch {
        icount: u64,
        expected: String,
        actual: String,
    },
    #[error("checkpoint line {0:?} does not start with an icount")]
    MalformedCheckpoint(String),
    #[error(
        "expected to have committed batch_id={expected}, but batch_commit's latest is {found}: another runner is writing to this database, and flushing now would derive its batch instead of this one"
    )]
    BatchIdMoved { expected: u64, found: u64 },
    #[error(
        "compared {compared} of {expected} register checkpoints in (icount {from}, {to}] -- a loop that silently skips boundaries is indistinguishable from one that compared them all unless this fires"
    )]
    CheckpointCountShortfall {
        compared: u64,
        expected: u64,
        from: u64,
        to: u64,
    },
    #[error(
        "frames_out holds {rows} rows ({distinct} distinct) over frame_no {lowest}..{highest}, which spans {} -- a frame's readout was lost or redone, and neither cpu_state nor a checkpoint reports it",
        highest - lowest + 1
    )]
    FramesOutGap {
        rows: u64,
        distinct: u64,
        lowest: u64,
        highest: u64,
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
    /// Register checkpoints compared against the reference trace, over the
    /// icount range this process covered rather than the whole run's.
    pub reg_checkpoints_compared: u64,
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

/// Derives `batch_id`'s row into `ram`, `framebuffer`, `palette`,
/// `console_out` and `cpu_state`, in that order. Idempotent, so a caller
/// may run it for a batch that has already been flushed.
async fn flush(db: &crate::client::Db, database: &str, batch_id: u64) -> Result<(), RunError> {
    db.run(&commit::ram_flush_sql(database, batch_id)).await?;
    // fbpal_flush_sql returns two statements (framebuffer, then palette) in
    // one string; the HTTP interface takes one statement per request.
    for statement in crate::sql::split_statements(&commit::fbpal_flush_sql(database, batch_id)) {
        db.run(statement).await?;
    }
    db.run(&commit::console_out_flush_sql(database, batch_id))
        .await?;
    db.run(&commit::cpu_state_flush_sql(database, batch_id))
        .await?;
    Ok(())
}

/// A trace line's first three fields, which is the whole line at the
/// register cadence and its register prefix at a `RAM_HASH_INTERVAL` one.
fn register_fields(line: &str) -> String {
    line.split('\t').take(3).collect::<Vec<_>>().join("\t")
}

/// Redoes every derivation of `batch_id`'s committed row: the four flushes,
/// and the `frames_out` readout when that batch committed a frame. Every
/// one of them is safe to redo, so a caller runs this for the last
/// committed batch before any new one rather than deciding whether it is
/// needed.
pub async fn recover(
    db: &crate::client::Db,
    database: &str,
    batch_id: u64,
) -> Result<(), RunError> {
    flush(db, database, batch_id).await?;
    db.run(&render::frame_readout_sql(database, batch_id))
        .await?;
    Ok(())
}

/// Compares every register checkpoint `batch_id` recorded against the
/// reference trace, in icount order, and returns how many it compared.
/// Stops at the first line that differs.
async fn compare_batch_checkpoints(
    db: &crate::client::Db,
    database: &str,
    batch_id: u64,
    trace_path: &Path,
) -> Result<u64, RunError> {
    let actual_lines: Vec<String> = db
        .fetch_all(&batch_checkpoints_sql(database, batch_id))
        .await?;
    let mut compared = 0u64;
    for actual in actual_lines {
        let icount: u64 = actual
            .split('\t')
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| RunError::MalformedCheckpoint(actual.clone()))?;
        let expected = register_fields(&trace_line_for(trace_path, icount)?);
        if actual != expected {
            return Err(RunError::CheckpointMismatch {
                icount,
                expected,
                actual,
            });
        }
        compared += 1;
    }
    Ok(compared)
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

    // `batch_commit` is the authority on where a run has got to: it is the
    // batch's single atomic write, and everything else is derived from it.
    // Redoing the last row's derivations before any new batch is the
    // recovery step, so a crash between that write and any of them cannot
    // leave `ram` short of a batch, or `frames_out` short of a frame, that
    // the resume point counts as done. Every derivation is safe to redo, so
    // this costs one redo on the ordinary path.
    let (resume_batch, resume_icount): (u64, u64) = db
        .fetch_one("SELECT batch_id, icount FROM batch_commit ORDER BY batch_id DESC LIMIT 1")
        .await?;
    recover(&db, &conn.database, resume_batch).await?;
    eprintln!("# resuming from batch_id={resume_batch} icount={resume_icount}");

    let manifest = Manifest::read(args.manifest_path)?;
    let text_start = manifest.text_start.unwrap_or(RAM_BASE);
    let text_end = manifest.text_end.unwrap_or(RAM_BASE);
    let load_addr = manifest.load_addr.unwrap_or(RAM_BASE);
    let text_start_word = WordAddr::of_byte(text_start);
    let text_end_word = WordAddr::of_byte(text_end);
    let ram_base_word = WordAddr::of_byte(load_addr);
    let text_start_widx = text_start_word.widx_from(ram_base_word)?;
    let text_end_widx = text_end_word.widx_from(ram_base_word)?;
    let decn = text_end_word.get() - text_start_word.get();
    let ram_words = RAM_WORDS_DEFAULT;

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
    let mut reg_checkpoints_compared = 0u64;
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
        let this_batch = batch_id + 1;
        db.run(&batch_sql).await?;

        // The fold numbers its own row from the previous one, so the row it
        // just wrote is this batch only if nothing else wrote to this
        // database. A second runner moves `max(batch_id)` and every flush
        // below would then name its batch instead of ours, losing this
        // batch's write-log with no error anywhere.
        let (committed, new_icount, pc, halted, halt_reason, has_frame, frame_no): (
            u64,
            u64,
            u32,
            u8,
            String,
            u8,
            u32,
        ) = db
            .fetch_one(
                "SELECT batch_id, icount, pc, halted, halt_reason, has_frame, frame_no \
                 FROM batch_commit ORDER BY batch_id DESC LIMIT 1",
            )
            .await?;
        if committed != this_batch {
            return Err(RunError::BatchIdMoved {
                expected: this_batch,
                found: committed,
            });
        }

        flush(&db, &conn.database, this_batch).await?;
        db.run(&commit::retention_sql(
            &conn.database,
            this_batch,
            BATCH_COMMIT_RETENTION_N,
        ))
        .await?;

        batch_id = this_batch;
        icount = new_icount;
        batches_run += 1;
        eprintln!(
            "# batch_id={batch_id} icount={icount} pc={pc:#010x} halted={halted} halt_reason={halt_reason} has_frame={has_frame} frame_no={frame_no}"
        );
        if let Some(line) = stats.tick(progress(icount, batches_run, frames_observed)) {
            eprintln!("{line}");
        }

        reg_checkpoints_compared +=
            compare_batch_checkpoints(&db, &conn.database, batch_id, args.trace_path).await?;

        // A boundary landing on this batch's own last retired instruction
        // has no following step to record it inside the fold, so it is read
        // from the committed row instead. At a RAM_HASH_INTERVAL boundary
        // that row also carries `ramhash` and `fbhash`.
        if icount.is_multiple_of(CHECKPOINT_INTERVAL) && icount > 0 {
            let at_ram_hash = icount.is_multiple_of(ram_hash_interval);
            let actual: String = if at_ram_hash {
                db.fetch_one(&checkpoint_sql(&conn.database)).await?
            } else {
                db.fetch_one(&reg_checkpoint_sql(&conn.database)).await?
            };
            let line = trace_line_for(args.trace_path, icount)?;
            let expected = if at_ram_hash {
                line
            } else {
                register_fields(&line)
            };
            if actual != expected {
                return Err(RunError::CheckpointMismatch {
                    icount,
                    expected,
                    actual,
                });
            }
            reg_checkpoints_compared += 1;
            eprintln!("# checkpoint OK at icount={icount}");
        }

        if halted == 1 {
            halted_reason = halt_reason;
            break;
        }
        if has_frame == 1 {
            frames_observed += 1;
            db.run(&render::frame_readout_sql(&conn.database, batch_id))
                .await?;
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

    // A frame lost to a crash before this run started leaves a hole that
    // nothing else reports: `cpu_state` is consistent, every checkpoint
    // passes, and `frames_out` is simply one row short.
    let (rows, distinct, lowest, highest): (u64, u64, u64, u64) = db
        .fetch_one(&render::frames_out_span_sql(&conn.database))
        .await?;
    if rows > 0 {
        let span = highest - lowest + 1;
        if rows != span || distinct != span {
            return Err(RunError::FramesOutGap {
                rows,
                distinct,
                lowest,
                highest,
            });
        }
        eprintln!("# frames_out holds {rows} rows over frame_no {lowest}..{highest}");
    }

    // Every boundary between where this process resumed and where it
    // stopped had to produce exactly one comparison. Without this, a fold
    // that recorded nothing reads as a clean run.
    let reg_checkpoints_expected =
        icount / CHECKPOINT_INTERVAL - resume_icount / CHECKPOINT_INTERVAL;
    if reg_checkpoints_compared != reg_checkpoints_expected {
        return Err(RunError::CheckpointCountShortfall {
            compared: reg_checkpoints_compared,
            expected: reg_checkpoints_expected,
            from: resume_icount,
            to: icount,
        });
    }
    eprintln!(
        "# compared {reg_checkpoints_compared} register checkpoints in (icount {resume_icount}, {icount}]"
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
        reg_checkpoints_compared,
    })
}
