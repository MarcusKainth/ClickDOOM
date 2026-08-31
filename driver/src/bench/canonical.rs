//! The canonical real-ROM throughput benchmark: two windows (a fresh boot,
//! and a store-heavy gameplay stretch reached via a cached `refemu`
//! snapshot rather than tens of hours of live execution), each measured
//! fold-alone and end to end.
//!
//! Reports two windows separately rather than blending them into one
//! average: a correctness check that costs more on memory-heavy code than
//! on the instruction stream generally would otherwise wash out in a
//! whole-run number.
//!
//! Each of the four arms runs against a container of its own. ClickHouse
//! counts executions of an expression DAG in a process-static map, and
//! `fold::select_only` and `fold::batch` emit the same step lambda, so two
//! arms sharing a server share one counter and one compiled function: the
//! first to run pays for the compilation and the second collects it. No
//! `SYSTEM` statement resets that counter, so a fresh server process is the
//! only thing that does.
//!
//! Each arm runs `warmup` batches before it times anything, and every batch
//! carries the compilation it did (`CompileFunction`,
//! `CompileExpressionsMicroseconds`), its write-log length, its retired
//! count and why it stopped. A run is refused unless the warm-up compiled
//! something and no timed batch compiled anything.

use std::path::{Path, PathBuf};
use std::time::Instant; // purity-ok: timing the benchmark harness itself, never a value the emulated machine computes with

use clickdoom_executor::commit;
use clickdoom_executor::config::{BATCH_COMMIT_RETENTION_N, HALT_REASON_NAMES};
use clickdoom_executor::fold::{self, BatchArgs, SelectOnlyArgs};
use clickdoom_spec::{Manifest, RAM_BASE, sha256_hex};
use refemu::snapshot::{Kind, Snapshot};
use serde::{Deserialize, Serialize};

use super::regime::{self, Regime};
use crate::bootstrap::BatchCommitRow;
use crate::client::{ConnArgs, Db, Error};
use crate::fold_result::FoldResult;
use crate::preflight::{PINNED_HASH, SCHEMA_SQL};
use crate::render::{FRAMEBUFFER_WORDS, PALETTE_WORDS};
use crate::rom::{RAM_WORDS_DEFAULT, WordRow};
use crate::sql::split_statements;

/// The database each arm provisions on its own server.
const ARM_DATABASE: &str = "canonical_throughput";

#[derive(Debug, thiserror::Error)]
pub enum CanonicalError {
    #[error(
        "{window} {mode} timed batch {batch}: the write log reached the high-water mark ({hwm}) after {retired} of K={k} instructions. A truncated batch measures different work than a full one. That the mark binds on this window's real store density is itself a finding: report it, don't paper over it by lowering K or raising HWM without saying so."
    )]
    HighWaterMarkBound {
        window: String,
        mode: &'static str,
        batch: u32,
        retired: u64,
        k: u32,
        hwm: u32,
    },
    #[error(
        "{window} {mode} batch {batch}: retired {retired}, not K={k}. It did not halt, it committed no frame, and its write log holds {write_log_len} of HWM={hwm}. Nothing else ends a batch early, so either the batch execution contract moved or this is reading the wrong columns."
    )]
    UnexplainedShortBatch {
        window: String,
        mode: &'static str,
        batch: u32,
        retired: u64,
        k: u32,
        hwm: u32,
        write_log_len: u32,
    },
    #[error(
        "{window} {mode}: none of the {warmup} warm-up batches compiled anything, so nothing proves the timed batches ran compiled. Expect CompileFunction > 0 on a warm-up batch of a freshly started server. Either the server has compilation off, or this arm's server was not fresh."
    )]
    WarmUpCompiledNothing {
        window: String,
        mode: &'static str,
        warmup: u32,
    },
    #[error(
        "{window} {mode} timed batch {batch}: CompileFunction={compile_function}, {compile_micros} us spent compiling. A batch that pays for compilation measures different work than the ones around it. Raise --warmup above {warmup}."
    )]
    CompiledWhileTimed {
        window: String,
        mode: &'static str,
        batch: u32,
        compile_function: u64,
        compile_micros: u64,
        warmup: u32,
    },
    #[error(
        "{window} {mode} batch {batch}: system.query_log has no QueryFinish row for {query_id}, so this batch's compilation regime is unknown. A number without its regime is not comparable to another number."
    )]
    NoQueryLogRow {
        window: String,
        mode: &'static str,
        batch: u32,
        query_id: String,
    },
    #[error(
        "{window} {mode} batch {batch}: the machine halted ({reason}) after {retired} instructions. A window that halts partway has no throughput to report."
    )]
    Halted {
        window: String,
        mode: &'static str,
        batch: u32,
        retired: u64,
        reason: String,
    },
    #[error(
        "{window}: the two arms diverged. After the same batches fold-alone is at pc {fold_pc:#010x} icount {fold_icount}, end to end at pc {e2e_pc:#010x} icount {e2e_icount}. Both execute the same instruction stream from the same start, so a difference means one arm read state the other wrote."
    )]
    ArmsDiverged {
        window: String,
        fold_pc: u32,
        fold_icount: u64,
        e2e_pc: u32,
        e2e_icount: u64,
    },
    #[error("{0}: sha256 ({1}) != rom/PINNED_HASH ({2})")]
    RomHash(PathBuf, String, String),
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Manifest(#[from] clickdoom_spec::manifest::ManifestError),
    #[error(transparent)]
    Snapshot(#[from] refemu::snapshot::SnapshotError),
    #[error("running {0}: {1}")]
    Spawn(String, std::io::Error),
    #[error("{0} exited with {1}")]
    RefemuFailed(String, std::process::ExitStatus),
    #[error("starting a container from {0}: {1}")]
    DockerStart(String, Box<super::docker::DockerError>),
    #[error("snapshot is a {0:?} capture, not a whole machine")]
    NotAMachineSnapshot(refemu::snapshot::Kind),
    #[error("snapshot ram is {0} bytes, not a multiple of 4")]
    RamNotWordAligned(usize),
    #[error("snapshot {0} is {1} bytes, expected {2}")]
    WrongSectionSize(&'static str, usize, u32),
    #[error("snapshot regs has {0} elements, expected 32 (x0..x31)")]
    WrongRegisterCount(usize),
    #[error(
        "seeded {table} is not dense: {rows} rows spanning {span} words from {lowest}, expected {expected} rows spanning {expected} from {base_word}. Readers of this table index positionally."
    )]
    SeedNotDense {
        table: String,
        rows: u64,
        span: u64,
        lowest: u64,
        expected: u64,
        base_word: u32,
    },
    #[error(transparent)]
    Db(#[from] Error),
    #[error(transparent)]
    Bootstrap(#[from] crate::bootstrap::SeedError),
}

/// Which half of the measurement an arm runs.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Mode {
    /// `fold::select_only`, the `arrayFold` step expression alone.
    Fold,
    /// `fold::batch` plus every `commit` flush, the cost a real run pays.
    E2e,
}

impl Mode {
    /// Both arms, in the order a run measures them.
    pub const ALL: [Mode; 2] = [Mode::Fold, Mode::E2e];

    pub const fn label(self) -> &'static str {
        match self {
            Mode::Fold => "fold",
            Mode::E2e => "e2e",
        }
    }
}

/// Why a batch stopped. The batch execution contract ends a batch on a
/// halt, on the write-log high-water mark, or on a FRAME_COMMIT store, and
/// otherwise when K instructions have retired.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// It retired K instructions.
    FullK,
    /// The machine halted.
    Halt,
    /// A FRAME_COMMIT store ended it.
    FrameCommit,
    /// The write log reached the high-water mark.
    HighWaterMark,
}

impl Stop {
    pub const fn label(self) -> &'static str {
        match self {
            Stop::FullK => "full_k",
            Stop::Halt => "halt",
            Stop::FrameCommit => "frame_commit",
            Stop::HighWaterMark => "high_water_mark",
        }
    }
}

/// One batch, warm-up or timed, with everything a reader needs to say
/// whether its seconds are comparable to another batch's.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchRecord {
    /// 1-based, counting the warm-up batches.
    pub index: u32,
    /// Whether this batch's seconds and retired count are in the arm total.
    pub timed: bool,
    pub seconds: f64,
    pub retired: u64,
    /// `length(wl_addr)` at the end of the batch.
    pub write_log_len: u32,
    pub stop: Stop,
    pub regime: Regime,
}

/// One window, one mode, summed over the timed batches.
pub struct ArmResult {
    pub retired: u64,
    pub seconds: f64,
    /// Every batch the arm ran, warm-up first.
    pub batches: Vec<BatchRecord>,
    /// Where the chain ended. Both arms of a window execute the same
    /// instruction stream, so both land here.
    pub final_pc: u32,
    pub final_icount: u64,
}

impl ArmResult {
    pub fn instr_per_sec(&self) -> f64 {
        self.retired as f64 / self.seconds
    }
}

pub struct WindowResult {
    pub label: String,
    pub k: u32,
    pub hwm: u32,
    pub fold: ArmResult,
    pub e2e: ArmResult,
}

pub struct Report {
    pub rom_sha256: String,
    pub clickhouse_version: String,
    pub image: String,
    pub decoded_rows: u64,
    pub k: u32,
    pub hwm: u32,
    pub warmup: u32,
    pub batches: u32,
    pub git_sha: String,
    pub windows: Vec<WindowResult>,
}

/// Where the gameplay window starts.
pub struct Windows {
    /// The icount the cached snapshot captures.
    pub gameplay_target_icount: u64,
}

impl Default for Windows {
    fn default() -> Self {
        Windows {
            gameplay_target_icount: 233_932_753,
        }
    }
}

pub struct Args<'a> {
    pub bin: &'a Path,
    pub manifest_path: &'a Path,
    /// The image each arm's own container starts from.
    pub image: &'a str,
    pub k: u32,
    pub hwm: u32,
    /// Batches each arm runs before it times anything.
    pub warmup: u32,
    /// Timed batches per arm.
    pub batches: u32,
    pub windows: Windows,
    pub snapshot_dir: PathBuf,
    pub refemu_bin: PathBuf,
}

fn wrap_regs(regs: &[u32]) -> Vec<String> {
    // Wrapped per element as `toUInt32(N)`, not passed as bare numbers: an
    // all-small array (the reset vector's 31 zeros, or a snapshot's own
    // small register values) is otherwise inferred as `Array(UInt8)`, and
    // `arrayFold` then rejects it as a type mismatch against the UInt32
    // accumulator.
    regs.iter().map(|r| format!("toUInt32({r})")).collect()
}

pub(crate) async fn create_and_load_database(
    db: &Db,
    conn: &ConnArgs,
    database: &str,
    bin: &Path,
    manifest_path: &Path,
    text_start_word: u32,
    text_end_word: u32,
) -> Result<(), CanonicalError> {
    db.run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await?;
    db.run(&format!("CREATE DATABASE {database}")).await?;
    let qualified_schema = SCHEMA_SQL
        .replace("clickdoom.", &format!("{database}."))
        .replace(
            "CREATE DATABASE IF NOT EXISTS clickdoom;",
            &format!("CREATE DATABASE IF NOT EXISTS {database};"),
        );
    for statement in split_statements(&qualified_schema) {
        db.run(statement).await?;
    }
    let mut window_conn = conn.clone();
    window_conn.database = database.to_string();
    let loaded_db = window_conn.connect();
    crate::rom::load(&loaded_db, bin, manifest_path, RAM_WORDS_DEFAULT)
        .await
        .map_err(|e| CanonicalError::Read {
            path: bin.to_owned(),
            source: std::io::Error::other(e.to_string()),
        })?;
    crate::decode::decode(&loaded_db, database, text_start_word, text_end_word).await?;
    Ok(())
}

/// The shape every batch against one window shares: everything
/// `select_only`/`batch` need beyond K and the seed pc/regs.
struct Shape<'a> {
    k: u32,
    hwm: u32,
    text_start_widx: u32,
    text_end_widx: u32,
    decn: u32,
    ram_words: u32,
    database: &'a str,
}

/// What one batch did, before its compilation regime is read back.
struct BatchOutcome {
    seconds: f64,
    retired: u64,
    pc: u32,
    halted: u8,
    halt_reason: String,
    write_log_len: u32,
    frame_committed: u8,
    /// Every statement inside the timed region, in the order it ran.
    query_ids: Vec<String>,
}

/// Where a batch's `query_id`s come from, so `system.query_log` can be read
/// back for exactly the statements this batch timed.
struct Tag<'a> {
    run: u32,
    window: &'a str,
    mode: &'static str,
    batch: u32,
}

impl Tag<'_> {
    fn statement(&self, n: usize) -> String {
        regime::query_id(self.run, self.window, self.mode, self.batch, n)
    }
}

/// Where a chained arm has got to. `icount` and `keyq` are threaded as well
/// as `pc`/`regs`: emulated time is a function of the retired count, so a
/// batch restarted at icount 0 executes a different instruction stream.
struct Chain {
    pc: u32,
    regs: Vec<String>,
    icount: u64,
    keyq: u32,
}

async fn run_fold_batch(
    db: &Db,
    shape: &Shape<'_>,
    tag: &Tag<'_>,
    chain: &Chain,
) -> Result<(BatchOutcome, FoldResult), Error> {
    let args = SelectOnlyArgs {
        pc0: Some(chain.pc),
        regs0: Some(&chain.regs),
        db: shape.database,
        icount0: chain.icount,
        keyq0: chain.keyq,
        ..Default::default()
    };
    let sql = fold::select_only(
        shape.k,
        shape.text_start_widx,
        shape.text_end_widx,
        shape.decn,
        shape.ram_words,
        shape.hwm,
        &args,
    );
    let query_id = tag.statement(0);
    let t0 = Instant::now(); // purity-ok: timing this batch for the report, not used in any query
    let result: FoldResult = db.fetch_one_with_query_id(&query_id, &sql).await?;
    let seconds = t0.elapsed().as_secs_f64();
    let outcome = BatchOutcome {
        seconds,
        retired: result.retired as u64,
        pc: result.pc,
        halted: result.halted,
        halt_reason: halt_reason_name(result.halt_reason),
        write_log_len: result.wl_addr.len() as u32,
        frame_committed: result.frame_committed,
        query_ids: vec![query_id],
    };
    Ok((outcome, result))
}

/// The name `fold::batch` would put in `halt_reason`, for a fold-alone
/// result that carries the raw code instead. Both arms then report a halt
/// the same way.
fn halt_reason_name(code: u8) -> String {
    HALT_REASON_NAMES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| (*name).to_string())
        .unwrap_or_default()
}

/// Applies a fold-alone batch's write logs, the way `commit`'s flushes do
/// for the end-to-end arm. Untimed, and outside the fold statement: the
/// next chained batch has to read what this one wrote, and a fold that
/// reads stale RAM executes a different instruction stream.
async fn flush_fold_write_logs(db: &Db, result: &FoldResult) -> Result<(), Error> {
    // `wl_addr` is RAM_BASE-relative and `ram.word_addr` is absolute;
    // `fb_wl_addr`/`pal_wl_addr` are already relative to their own region's
    // base. Same asymmetry `commit::ram_flush_sql` and
    // `commit::fbpal_flush_sql` carry.
    insert_words(
        db,
        "ram",
        RAM_BASE >> 2,
        &result.wl_addr,
        &result.wl_val,
        &result.wl_icount,
    )
    .await?;
    insert_words(
        db,
        "framebuffer",
        0,
        &result.fb_wl_addr,
        &result.fb_wl_val,
        &result.fb_wl_icount,
    )
    .await?;
    insert_words(
        db,
        "palette",
        0,
        &result.pal_wl_addr,
        &result.pal_wl_val,
        &result.pal_wl_icount,
    )
    .await?;
    Ok(())
}

async fn insert_words(
    db: &Db,
    table: &str,
    base_word: u32,
    addr: &[u32],
    val: &[u32],
    icount: &[u64],
) -> Result<(), Error> {
    if addr.is_empty() {
        return Ok(());
    }
    let rows = addr
        .iter()
        .zip(val)
        .zip(icount)
        .map(|((&word, &value), &version)| WordRow {
            word_addr: base_word + word,
            value,
            version,
        });
    db.insert_all(table, rows).await
}

async fn run_e2e_batch(db: &Db, shape: &Shape<'_>, tag: &Tag<'_>) -> Result<BatchOutcome, Error> {
    let database = shape.database;
    let cpu_state_rows: u64 = db
        .fetch_one(&format!("SELECT count() FROM {database}.cpu_state"))
        .await?;
    let prev_icount: u64 = if cpu_state_rows == 0 {
        db.fetch_one(&format!(
            "SELECT icount FROM {database}.batch_commit ORDER BY batch_id DESC LIMIT 1"
        ))
        .await?
    } else {
        db.fetch_one(&format!(
            "SELECT icount FROM {database}.cpu_state ORDER BY batch_id DESC LIMIT 1"
        ))
        .await?
    };
    let batch_args = BatchArgs {
        db: database,
        ..Default::default()
    };
    let batch_sql = fold::batch(
        shape.k,
        shape.text_start_widx,
        shape.text_end_widx,
        shape.decn,
        shape.ram_words,
        shape.hwm,
        &batch_args,
    );
    let statements = [
        batch_sql,
        commit::ram_flush_sql(database),
        commit::console_out_flush_sql(database),
        commit::cpu_state_flush_sql(database),
        commit::retention_sql(database, BATCH_COMMIT_RETENTION_N),
    ];
    let query_ids: Vec<String> = (0..statements.len()).map(|n| tag.statement(n)).collect();
    let t0 = Instant::now(); // purity-ok: timing this batch for the report, not used in any query
    for (sql, query_id) in statements.iter().zip(&query_ids) {
        db.run_with_query_id(query_id, sql).await?;
    }
    let seconds = t0.elapsed().as_secs_f64();
    let (icount, pc, halted, halt_reason): (u64, u32, u8, String) = db
        .fetch_one(&format!(
            "SELECT icount, pc, halted, halt_reason FROM {database}.cpu_state ORDER BY batch_id DESC LIMIT 1"
        ))
        .await?;
    let (has_frame, write_log_len): (u8, u32) = db
        .fetch_one(&format!(
            "SELECT has_frame, toUInt32(length(wl_addr)) FROM {database}.batch_commit ORDER BY batch_id DESC LIMIT 1"
        ))
        .await?;
    Ok(BatchOutcome {
        seconds,
        retired: icount - prev_icount,
        pc,
        halted,
        halt_reason,
        write_log_len,
        frame_committed: has_frame,
        query_ids,
    })
}

/// Names why a batch stopped. `None` means nothing in the batch execution
/// contract accounts for it.
fn classify(outcome: &BatchOutcome, k: u32, hwm: u32) -> Option<Stop> {
    if outcome.halted != 0 {
        return Some(Stop::Halt);
    }
    if outcome.retired == u64::from(k) {
        return Some(Stop::FullK);
    }
    if outcome.frame_committed != 0 {
        return Some(Stop::FrameCommit);
    }
    if outcome.write_log_len >= hwm {
        return Some(Stop::HighWaterMark);
    }
    None
}

struct ArmArgs<'a> {
    window: &'a str,
    mode: Mode,
    warmup: u32,
    batches: u32,
    run: u32,
    start: Chain,
}

/// Runs one arm: `warmup` batches, then `batches` timed ones, chained
/// through the same state the untimed ones left.
async fn run_arm(
    db: &Db,
    shape: &Shape<'_>,
    args: &ArmArgs<'_>,
) -> Result<ArmResult, CanonicalError> {
    let mode = args.mode.label();
    let window = args.window;
    eprintln!(
        "# {window} {mode}: {} warm-up + {} timed batches of K={}, chained",
        args.warmup, args.batches, shape.k
    );
    let mut chain = Chain {
        pc: args.start.pc,
        regs: args.start.regs.clone(),
        icount: args.start.icount,
        keyq: args.start.keyq,
    };
    let mut result = ArmResult {
        retired: 0,
        seconds: 0.0,
        batches: Vec::new(),
        final_pc: chain.pc,
        final_icount: chain.icount,
    };
    let mut query_ids: Vec<(u32, Vec<String>)> = Vec::new();

    for index in 1..=(args.warmup + args.batches) {
        let timed = index > args.warmup;
        let tag = Tag {
            run: args.run,
            window,
            mode,
            batch: index,
        };
        let outcome = match args.mode {
            Mode::Fold => {
                let (outcome, fold_result) = run_fold_batch(db, shape, &tag, &chain).await?;
                flush_fold_write_logs(db, &fold_result).await?;
                chain.pc = fold_result.pc;
                chain.regs = wrap_regs(&fold_result.regs);
                chain.icount += outcome.retired;
                chain.keyq = fold_result.keyq_pos;
                outcome
            }
            Mode::E2e => run_e2e_batch(db, shape, &tag).await?,
        };
        let stop = classify(&outcome, shape.k, shape.hwm).ok_or_else(|| {
            CanonicalError::UnexplainedShortBatch {
                window: window.to_string(),
                mode,
                batch: index,
                retired: outcome.retired,
                k: shape.k,
                hwm: shape.hwm,
                write_log_len: outcome.write_log_len,
            }
        })?;
        if stop == Stop::Halt {
            return Err(CanonicalError::Halted {
                window: window.to_string(),
                mode,
                batch: index,
                retired: outcome.retired,
                reason: outcome.halt_reason,
            });
        }
        // A warm-up batch only has to advance the chain, so the mark
        // binding on it is reported and allowed. A timed one measures
        // different work than a full batch, so it is refused.
        if timed && stop == Stop::HighWaterMark {
            return Err(CanonicalError::HighWaterMarkBound {
                window: window.to_string(),
                mode,
                batch: index,
                retired: outcome.retired,
                k: shape.k,
                hwm: shape.hwm,
            });
        }
        eprintln!(
            "#   {} batch {index}: {:.2}s retired={} wl={} stop={}",
            if timed { "timed " } else { "warm-up" },
            outcome.seconds,
            outcome.retired,
            outcome.write_log_len,
            stop.label()
        );
        if timed {
            result.retired += outcome.retired;
            result.seconds += outcome.seconds;
        }
        result.final_pc = outcome.pc;
        result.final_icount += outcome.retired;
        query_ids.push((index, outcome.query_ids));
        result.batches.push(BatchRecord {
            index,
            timed,
            seconds: outcome.seconds,
            retired: outcome.retired,
            write_log_len: outcome.write_log_len,
            stop,
            regime: Regime::default(),
        });
    }

    attach_regime(db, window, mode, &query_ids, &mut result.batches).await?;
    check_regime(window, mode, args.warmup, &result.batches)?;
    eprintln!(
        "# {window} {mode} total: retired={} seconds={:.2} instr/sec={:.1}",
        result.retired,
        result.seconds,
        result.instr_per_sec()
    );
    Ok(result)
}

/// Fills in each batch's compilation events from `system.query_log`. A
/// batch whose statements left no row is an error: an absent row would
/// otherwise read as "compiled nothing", which is the thing the check is
/// looking for.
async fn attach_regime(
    db: &Db,
    window: &str,
    mode: &'static str,
    query_ids: &[(u32, Vec<String>)],
    batches: &mut [BatchRecord],
) -> Result<(), CanonicalError> {
    let flat: Vec<String> = query_ids
        .iter()
        .flat_map(|(_, ids)| ids.iter().cloned())
        .collect();
    let by_id = regime::read(db, &flat).await?;
    for ((batch, ids), record) in query_ids.iter().zip(batches.iter_mut()) {
        for id in ids {
            let found = by_id.get(id).ok_or_else(|| CanonicalError::NoQueryLogRow {
                window: window.to_string(),
                mode,
                batch: *batch,
                query_id: id.clone(),
            })?;
            record.regime.add(*found);
        }
    }
    Ok(())
}

/// Holds the arm's numbers to one compilation regime: the warm-up crossed
/// the compile threshold, and no timed batch crossed it. The second half
/// alone would pass on a server that never compiles at all, so both are
/// checked.
fn check_regime(
    window: &str,
    mode: &'static str,
    warmup: u32,
    batches: &[BatchRecord],
) -> Result<(), CanonicalError> {
    let warmed = batches
        .iter()
        .any(|b| !b.timed && b.regime.compile_function > 0);
    if !warmed {
        return Err(CanonicalError::WarmUpCompiledNothing {
            window: window.to_string(),
            mode,
            warmup,
        });
    }
    for batch in batches.iter().filter(|b| b.timed) {
        if batch.regime.compile_function > 0 {
            return Err(CanonicalError::CompiledWhileTimed {
                window: window.to_string(),
                mode,
                batch: batch.index,
                compile_function: batch.regime.compile_function,
                compile_micros: batch.regime.compile_micros,
                warmup,
            });
        }
    }
    Ok(())
}

async fn seed_word_table(
    db: &Db,
    table: &str,
    words: &[u32],
    base_word: u32,
) -> Result<(), CanonicalError> {
    let rows = words.iter().enumerate().map(|(i, &value)| WordRow {
        word_addr: base_word + i as u32,
        value,
        version: 0,
    });
    db.insert_all(table, rows).await?;
    let (rows_n, span, lowest): (u64, u64, u64) = db
        .fetch_one(&format!(
            "SELECT count(), toUInt64(max(word_addr) - min(word_addr) + 1), toUInt64(min(word_addr)) FROM {table} FINAL"
        ))
        .await?;
    if rows_n != span || rows_n != words.len() as u64 || lowest != base_word as u64 {
        return Err(CanonicalError::SeedNotDense {
            table: table.to_string(),
            rows: rows_n,
            span,
            lowest,
            expected: words.len() as u64,
            base_word,
        });
    }
    Ok(())
}

fn words_le(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect()
}

/// Seeds `ram`/`framebuffer`/`palette` and a `batch_commit` `batch_id = 0`
/// row from a captured machine, so a benchmark window can start from a
/// real, representative mid-run state without live-executing the
/// instructions it takes to reach it. `ram` is RAM_BASE-relative;
/// `framebuffer`/`palette` are each region's own base-relative convention
/// (base_word 0), matching `commit::fbpal_flush_sql`'s side of the same
/// asymmetry.
async fn seed_snapshot(db: &Db, snapshot: &Snapshot) -> Result<(), CanonicalError> {
    if snapshot.header.kind != Kind::Machine {
        return Err(CanonicalError::NotAMachineSnapshot(snapshot.header.kind));
    }
    let ram_bytes = snapshot.section("ram").expect("checked by Snapshot::read");
    if !ram_bytes.len().is_multiple_of(4) {
        return Err(CanonicalError::RamNotWordAligned(ram_bytes.len()));
    }
    let ram_base = snapshot.header.ram_base.unwrap_or(RAM_BASE);
    let ram_base_word = ram_base / 4;
    let ram_words = words_le(ram_bytes);
    seed_word_table(db, "ram", &ram_words, ram_base_word).await?;

    let fb_bytes = snapshot
        .section("framebuffer")
        .expect("checked by Snapshot::read");
    if fb_bytes.len() as u32 != FRAMEBUFFER_WORDS * 4 {
        return Err(CanonicalError::WrongSectionSize(
            "framebuffer",
            fb_bytes.len(),
            FRAMEBUFFER_WORDS * 4,
        ));
    }
    let pal_bytes = snapshot
        .section("palette")
        .expect("checked by Snapshot::read");
    if pal_bytes.len() as u32 != PALETTE_WORDS * 4 {
        return Err(CanonicalError::WrongSectionSize(
            "palette",
            pal_bytes.len(),
            PALETTE_WORDS * 4,
        ));
    }
    seed_word_table(db, "framebuffer", &words_le(fb_bytes), 0).await?;
    seed_word_table(db, "palette", &words_le(pal_bytes), 0).await?;

    let regs32 = snapshot
        .header
        .regs
        .as_ref()
        .expect("checked by Snapshot::read for a machine snapshot");
    if regs32.len() != 32 {
        return Err(CanonicalError::WrongRegisterCount(regs32.len()));
    }
    let row = BatchCommitRow {
        batch_id: 0,
        icount: snapshot.header.icount,
        pc: snapshot.header.pc.expect("checked above"),
        regs: regs32[1..32].to_vec(),
        halted: 0,
        halt_reason: String::new(),
        exit_code: 0,
        keyq_pos: 0,
        has_frame: 0,
        frame_no: 0,
        wl_addr: Vec::new(),
        wl_val: Vec::new(),
        wl_icount: Vec::new(),
        fb_wl_addr: Vec::new(),
        fb_wl_val: Vec::new(),
        fb_wl_icount: Vec::new(),
        pal_wl_addr: Vec::new(),
        pal_wl_val: Vec::new(),
        pal_wl_icount: Vec::new(),
        console_bytes: Vec::new(),
    };
    db.insert_all("batch_commit", std::iter::once(row)).await?;
    Ok(())
}

/// Runs `refemu run --stop-at icount:N --dump-state` to reach
/// `target_icount` in minutes instead of the tens of hours live execution
/// through the SQL CPU would cost, caching the result: the format version is
/// part of the filename, so a capture written by an older reader can never
/// be picked up by a newer one.
fn generate_or_reuse_snapshot(
    refemu_bin: &Path,
    bin: &Path,
    manifest_path: &Path,
    rom_sha256: &str,
    target_icount: u64,
    snapshot_dir: &Path,
) -> Result<PathBuf, CanonicalError> {
    let rom_prefix = &rom_sha256[..12.min(rom_sha256.len())];
    let path = snapshot_dir.join(format!(
        "snapshot.{rom_prefix}.{target_icount}.v{}.rsnap",
        refemu::snapshot::FORMAT_VERSION
    ));
    if path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(snapshot_dir).map_err(|source| CanonicalError::Read {
        path: snapshot_dir.to_owned(),
        source,
    })?;
    let pinned_hash_path = Path::new("rom/PINNED_HASH");
    let status = std::process::Command::new(refemu_bin)
        .arg("run")
        .arg(bin)
        .arg("--manifest")
        .arg(manifest_path)
        .arg("--pinned-hash")
        .arg(pinned_hash_path)
        .arg("--stop-at")
        .arg(format!("icount:{target_icount}"))
        .arg("--max-instructions")
        .arg(target_icount.to_string())
        .arg("--dump-state")
        .arg(&path)
        .status()
        .map_err(|e| CanonicalError::Spawn(refemu_bin.display().to_string(), e))?;
    if !status.success() {
        return Err(CanonicalError::RefemuFailed(
            refemu_bin.display().to_string(),
            status,
        ));
    }
    Ok(path)
}

/// Where a window's start state comes from.
enum Seed<'a> {
    /// The reset vector.
    Reset,
    /// A captured machine.
    Snapshot(&'a Snapshot),
}

/// One window: its label, its start state, and where it starts executing.
struct Window<'a> {
    label: String,
    seed: Seed<'a>,
    start: Chain,
}

/// Runs the whole benchmark: both windows, fold-alone and end to end, each
/// arm on a container of its own. A container is removed when its handle
/// drops, so an arm that fails leaves nothing behind.
pub async fn run(args: &Args<'_>) -> Result<Report, CanonicalError> {
    let blob = std::fs::read(args.bin).map_err(|source| CanonicalError::Read {
        path: args.bin.to_owned(),
        source,
    })?;
    let rom_sha256 = sha256_hex(&blob);
    let pinned = PINNED_HASH.trim();
    if rom_sha256 != pinned {
        return Err(CanonicalError::RomHash(
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

    let git_sha = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .map_err(|e| CanonicalError::Spawn("git".to_string(), e))?
            .stdout,
    )
    .unwrap_or_default()
    .trim()
    .to_string();

    eprintln!(
        "# --- generating/reusing gameplay-window snapshot (icount={}) ---",
        args.windows.gameplay_target_icount
    );
    let snapshot_path = generate_or_reuse_snapshot(
        &args.refemu_bin,
        args.bin,
        args.manifest_path,
        &rom_sha256,
        args.windows.gameplay_target_icount,
        &args.snapshot_dir,
    )?;
    let snapshot = Snapshot::read(&snapshot_path, &["ram", "framebuffer", "palette"])?;
    let snapshot_frame = snapshot
        .header
        .last_frame_commit
        .as_ref()
        .map(|f| format!("after frame index {} (frame_no {})", f.index, f.frame_no))
        .unwrap_or_else(|| "before any frame".to_string());

    let windows = [
        Window {
            label: "boot: from icount 0".to_string(),
            seed: Seed::Reset,
            start: Chain {
                pc: RAM_BASE,
                regs: wrap_regs(&[0u32; 31]),
                icount: 0,
                keyq: 0,
            },
        },
        Window {
            label: format!(
                "store-heavy gameplay: from icount {}, {snapshot_frame}",
                snapshot.header.icount
            ),
            seed: Seed::Snapshot(&snapshot),
            start: Chain {
                pc: snapshot.header.pc.expect("machine snapshot carries pc"),
                regs: wrap_regs(
                    &snapshot
                        .header
                        .regs
                        .as_ref()
                        .expect("machine snapshot carries regs")[1..32],
                ),
                icount: snapshot.header.icount,
                keyq: 0,
            },
        },
    ];

    let run_id = std::process::id();
    let mut clickhouse_version = String::new();
    let mut decoded_rows = 0u64;
    let mut results = Vec::new();
    for window in &windows {
        let mut arms = Vec::new();
        for mode in Mode::ALL {
            eprintln!(
                "# === {} {} : starting a container from {} ===",
                window.label,
                mode.label(),
                args.image
            );
            let container = super::docker::start(args.image)
                .await
                .map_err(|e| CanonicalError::DockerStart(args.image.to_string(), Box::new(e)))?;
            let conn = ConnArgs {
                host: "127.0.0.1".to_string(),
                port: container.http_port,
                user: "default".to_string(),
                database: "default".to_string(),
                password: Some("clickdoom".to_string()),
            };
            let arm = provision_and_run(
                &conn,
                args,
                window,
                mode,
                run_id,
                text_start_word,
                text_end_word,
                Shape {
                    k: args.k,
                    hwm: args.hwm,
                    text_start_widx,
                    text_end_widx,
                    decn,
                    ram_words,
                    database: ARM_DATABASE,
                },
                &mut clickhouse_version,
                &mut decoded_rows,
            )
            .await;
            drop(container);
            arms.push(arm?);
        }
        let mut arms = arms.into_iter();
        let fold = arms.next().expect("one arm per mode");
        let e2e = arms.next().expect("one arm per mode");
        if fold.final_pc != e2e.final_pc || fold.final_icount != e2e.final_icount {
            return Err(CanonicalError::ArmsDiverged {
                window: window.label.clone(),
                fold_pc: fold.final_pc,
                fold_icount: fold.final_icount,
                e2e_pc: e2e.final_pc,
                e2e_icount: e2e.final_icount,
            });
        }
        eprintln!(
            "# {}: both arms end at pc {:#010x} icount {}",
            window.label, fold.final_pc, fold.final_icount
        );
        results.push(WindowResult {
            label: window.label.clone(),
            k: args.k,
            hwm: args.hwm,
            fold,
            e2e,
        });
    }

    Ok(Report {
        rom_sha256,
        clickhouse_version,
        image: args.image.to_string(),
        decoded_rows,
        k: args.k,
        hwm: args.hwm,
        warmup: args.warmup,
        batches: args.batches,
        git_sha,
        windows: results,
    })
}

#[allow(clippy::too_many_arguments)]
async fn provision_and_run(
    conn: &ConnArgs,
    args: &Args<'_>,
    window: &Window<'_>,
    mode: Mode,
    run_id: u32,
    text_start_word: u32,
    text_end_word: u32,
    shape: Shape<'_>,
    clickhouse_version: &mut String,
    decoded_rows: &mut u64,
) -> Result<ArmResult, CanonicalError> {
    let admin = db_at(conn, "default");
    *clickhouse_version = admin.fetch_one("SELECT version()").await?;
    create_and_load_database(
        &admin,
        conn,
        ARM_DATABASE,
        args.bin,
        args.manifest_path,
        text_start_word,
        text_end_word,
    )
    .await?;
    let db = db_at(conn, ARM_DATABASE);
    match window.seed {
        Seed::Reset => {
            crate::bootstrap::seed(&db, &crate::bootstrap::RESET_REGS).await?;
        }
        Seed::Snapshot(snapshot) => {
            db.run("TRUNCATE TABLE ram").await?;
            seed_snapshot(&db, snapshot).await?;
        }
    }
    *decoded_rows = db
        .fetch_one("SELECT count(DISTINCT word_addr) FROM decoded")
        .await?;
    run_arm(
        &db,
        &shape,
        &ArmArgs {
            window: &window.label,
            mode,
            warmup: args.warmup,
            batches: args.batches,
            run: run_id,
            start: Chain {
                pc: window.start.pc,
                regs: window.start.regs.clone(),
                icount: window.start.icount,
                keyq: window.start.keyq,
            },
        },
    )
    .await
}

pub(crate) fn db_at(conn: &ConnArgs, database: &str) -> Db {
    let mut c = conn.clone();
    c.database = database.to_string();
    c.connect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(retired: u64, halted: u8, frame: u8, wl: u32) -> BatchOutcome {
        BatchOutcome {
            seconds: 1.0,
            retired,
            pc: RAM_BASE,
            halted,
            halt_reason: String::new(),
            write_log_len: wl,
            frame_committed: frame,
            query_ids: Vec::new(),
        }
    }

    #[test]
    fn a_full_batch_is_full_k() {
        assert_eq!(
            classify(&outcome(60_000, 0, 0, 100), 60_000, 20_000),
            Some(Stop::FullK)
        );
    }

    #[test]
    fn a_halt_outranks_the_retired_count() {
        assert_eq!(
            classify(&outcome(113, 1, 0, 3), 60_000, 20_000),
            Some(Stop::Halt)
        );
    }

    #[test]
    fn a_short_batch_that_committed_a_frame_is_a_frame_commit() {
        // The case #241 reports: 10,942 retired, halted=0, and far too few
        // stores for the high-water mark to be the cause.
        assert_eq!(
            classify(&outcome(10_942, 0, 1, 42), 60_000, 20_000),
            Some(Stop::FrameCommit)
        );
    }

    #[test]
    fn a_short_batch_at_the_high_water_mark_is_named() {
        assert_eq!(
            classify(&outcome(31_500, 0, 0, 20_000), 60_000, 20_000),
            Some(Stop::HighWaterMark)
        );
    }

    #[test]
    fn a_short_batch_with_no_cause_is_not_classified() {
        assert_eq!(classify(&outcome(31_500, 0, 0, 7), 60_000, 20_000), None);
    }

    fn record(index: u32, timed: bool, compile_function: u64) -> BatchRecord {
        BatchRecord {
            index,
            timed,
            seconds: 1.0,
            retired: 60_000,
            write_log_len: 100,
            stop: Stop::FullK,
            regime: Regime {
                compile_function,
                compile_micros: compile_function * 1_000,
            },
        }
    }

    #[test]
    fn a_warm_up_that_compiled_and_timed_batches_that_did_not_pass() {
        let batches = [
            record(1, false, 0),
            record(2, false, 0),
            record(3, false, 0),
            record(4, false, 3),
            record(5, true, 0),
            record(6, true, 0),
        ];
        assert!(check_regime("boot", "fold", 4, &batches).is_ok());
    }

    #[test]
    fn a_warm_up_that_compiled_nothing_is_refused() {
        // What a second arm on a shared server sees: the first arm already
        // compiled the step lambda, so this arm's warm-up finds it cached.
        let batches = [record(1, false, 0), record(2, true, 0)];
        let err = check_regime("boot", "e2e", 1, &batches).expect_err("must refuse");
        assert!(
            matches!(err, CanonicalError::WarmUpCompiledNothing { .. }),
            "{err}"
        );
    }

    #[test]
    fn compiling_inside_a_timed_batch_is_refused() {
        let batches = [record(1, false, 3), record(2, true, 1)];
        let err = check_regime("boot", "fold", 1, &batches).expect_err("must refuse");
        assert!(
            matches!(err, CanonicalError::CompiledWhileTimed { batch: 2, .. }),
            "{err}"
        );
    }
}
