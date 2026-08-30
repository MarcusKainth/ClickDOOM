//! The canonical real-ROM throughput benchmark: two windows (a fresh boot,
//! and a store-heavy gameplay stretch reached via a cached `refemu`
//! snapshot rather than tens of hours of live execution), each measured
//! fold-alone and end to end, over a fixed number of chained batches.
//!
//! Reports two windows separately rather than blending them into one
//! average: a correctness check that costs more on memory-heavy code than
//! on the instruction stream generally would otherwise wash out in a
//! whole-run number.

use std::path::{Path, PathBuf};
use std::time::Instant; // purity-ok: timing the benchmark harness itself, never a value the emulated machine computes with

use clickdoom_executor::commit;
use clickdoom_executor::config::BATCH_COMMIT_RETENTION_N;
use clickdoom_executor::fold::{self, BatchArgs, SelectOnlyArgs};
use clickdoom_spec::{Manifest, RAM_BASE, sha256_hex};
use refemu::snapshot::{Kind, Snapshot};

use crate::bootstrap::BatchCommitRow;
use crate::client::{ConnArgs, Db, Error};
use crate::fold_result::FoldResult;
use crate::preflight::{PINNED_HASH, SCHEMA_SQL};
use crate::render::{FRAMEBUFFER_WORDS, PALETTE_WORDS};
use crate::rom::{RAM_WORDS_DEFAULT, WordRow};
use crate::sql::split_statements;

#[derive(Debug, thiserror::Error)]
pub enum CanonicalError {
    #[error(
        "{window} {mode} batch {batch}: retired {retired}, not K={k}, and did not halt. A truncated batch measures different work than a full one. If this is the write-log high-water mark ({hwm}) binding on this window's real store density, that is itself a real finding: report it, don't paper over it by lowering K or raising HWM without saying so."
    )]
    Truncated {
        window: String,
        mode: &'static str,
        batch: u32,
        retired: u64,
        k: u32,
        hwm: u32,
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

/// The two windows this benchmark always measures, in order.
pub struct Windows {
    /// Fresh reset state: `[0, boot_end_icount)`.
    pub boot_end_icount: u64,
    /// Where the cached snapshot starts the gameplay window.
    pub gameplay_target_icount: u64,
    /// Where the gameplay window ends, for the report label only: this
    /// names the slice of a real `demo3` playback the snapshot represents
    /// (frames 200 to 299 of the `e7_memfns` profile), not a bound this
    /// benchmark itself measures up to.
    pub gameplay_window_end_icount: u64,
}

impl Default for Windows {
    fn default() -> Self {
        Windows {
            boot_end_icount: 15_393_136,
            gameplay_target_icount: 233_932_753,
            gameplay_window_end_icount: 392_488_489,
        }
    }
}

pub struct Args<'a> {
    pub bin: &'a Path,
    pub manifest_path: &'a Path,
    pub k: u32,
    pub hwm: u32,
    pub batches: u32,
    pub windows: Windows,
    pub snapshot_dir: PathBuf,
    pub refemu_bin: PathBuf,
}

/// One window, one mode (fold-alone or end to end), summed over every
/// chained batch.
pub struct ModeResult {
    pub retired: u64,
    pub seconds: f64,
}

impl ModeResult {
    pub fn instr_per_sec(&self) -> f64 {
        self.retired as f64 / self.seconds
    }
}

pub struct WindowResult {
    pub label: String,
    pub k: u32,
    pub hwm: u32,
    pub fold: ModeResult,
    pub e2e: ModeResult,
    /// The last e2e batch's `halt_reason` (empty if it didn't halt).
    pub e2e_halt_reason: String,
}

pub struct Report {
    pub rom_sha256: String,
    pub decoded_rows: u64,
    pub k: u32,
    pub hwm: u32,
    pub batches: u32,
    pub git_sha: String,
    pub windows: Vec<WindowResult>,
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

async fn run_fold_batch(
    db: &Db,
    shape: &Shape<'_>,
    pc: u32,
    regs: &[String],
) -> Result<(FoldResult, f64), Error> {
    let args = SelectOnlyArgs {
        pc0: Some(pc),
        regs0: Some(regs),
        db: shape.database,
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
    let t0 = Instant::now(); // purity-ok: timing this batch for the report, not used in any query
    let result: FoldResult = db.fetch_one(&sql).await?;
    Ok((result, t0.elapsed().as_secs_f64()))
}

struct E2eResult {
    seconds: f64,
    retired: u64,
    halted: u8,
    halt_reason: String,
}

async fn run_e2e_batch(db: &Db, shape: &Shape<'_>) -> Result<E2eResult, Error> {
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
    let t0 = Instant::now(); // purity-ok: timing this batch for the report, not used in any query
    db.run(&batch_sql).await?;
    db.run(&commit::ram_flush_sql(database)).await?;
    db.run(&commit::console_out_flush_sql(database)).await?;
    db.run(&commit::cpu_state_flush_sql(database)).await?;
    db.run(&commit::retention_sql(database, BATCH_COMMIT_RETENTION_N))
        .await?;
    let seconds = t0.elapsed().as_secs_f64();
    let (icount, halted, halt_reason): (u64, u8, String) = db
        .fetch_one(&format!(
            "SELECT icount, halted, halt_reason FROM {database}.cpu_state ORDER BY batch_id DESC LIMIT 1"
        ))
        .await?;
    Ok(E2eResult {
        seconds,
        retired: icount - prev_icount,
        halted,
        halt_reason,
    })
}

async fn run_window(
    db: &Db,
    label: &str,
    shape: &Shape<'_>,
    batches: u32,
    pc0: u32,
    regs0: &[String],
) -> Result<WindowResult, CanonicalError> {
    let k = shape.k;
    let hwm = shape.hwm;
    eprintln!("# === window: {label} (database={}) ===", shape.database);
    eprintln!("# fold-alone: {batches} batches of K={k}, chained");
    let mut pc = pc0;
    let mut regs = regs0.to_vec();
    let mut fold = ModeResult {
        retired: 0,
        seconds: 0.0,
    };
    for batch_no in 1..=batches {
        let (result, seconds) = run_fold_batch(db, shape, pc, &regs).await?;
        if result.halted == 0 && result.retired != k {
            return Err(CanonicalError::Truncated {
                window: label.to_string(),
                mode: "fold-alone",
                batch: batch_no,
                retired: result.retired as u64,
                k,
                hwm,
            });
        }
        eprintln!(
            "#   fold batch {batch_no}: {seconds:.2}s retired={} halted={} halt_reason={}",
            result.retired, result.halted, result.halt_reason
        );
        fold.retired += result.retired as u64;
        fold.seconds += seconds;
        pc = result.pc;
        regs = wrap_regs(&result.regs);
    }
    eprintln!(
        "# fold-alone total: retired={} seconds={:.2} instr/sec={:.1}",
        fold.retired,
        fold.seconds,
        fold.instr_per_sec()
    );

    eprintln!("# e2e: {batches} batches of K={k}, chained through commit flushes");
    let mut e2e = ModeResult {
        retired: 0,
        seconds: 0.0,
    };
    let mut e2e_halt_reason = String::new();
    for batch_no in 1..=batches {
        let result = run_e2e_batch(db, shape).await?;
        if result.halted == 0 && result.retired != k as u64 {
            return Err(CanonicalError::Truncated {
                window: label.to_string(),
                mode: "e2e",
                batch: batch_no,
                retired: result.retired,
                k,
                hwm,
            });
        }
        eprintln!(
            "#   e2e batch {batch_no}: {:.2}s retired={} halted={} halt_reason={}",
            result.seconds, result.retired, result.halted, result.halt_reason
        );
        e2e.retired += result.retired;
        e2e.seconds += result.seconds;
        e2e_halt_reason = result.halt_reason;
    }
    eprintln!(
        "# e2e total: retired={} seconds={:.2} instr/sec={:.1}",
        e2e.retired,
        e2e.seconds,
        e2e.instr_per_sec()
    );

    Ok(WindowResult {
        label: label.to_string(),
        k,
        hwm,
        fold,
        e2e,
        e2e_halt_reason,
    })
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
        .chunks_exact(4)
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

/// Runs the whole benchmark: both windows, fold-alone and end to end,
/// `args.batches` chained batches each. The two per-run databases are
/// always dropped on the way out, success or failure, so an aborted run
/// never leaks one.
pub async fn run(conn: &ConnArgs, args: &Args<'_>) -> Result<Report, CanonicalError> {
    let boot_db = format!("canonical_throughput_boot_{}", std::process::id());
    let gameplay_db = format!("canonical_throughput_gameplay_{}", std::process::id());

    let result = run_inner(conn, args, &boot_db, &gameplay_db).await;

    let admin = db_at(conn, "default");
    let boot_dropped = admin
        .run(&format!("DROP DATABASE IF EXISTS {boot_db}"))
        .await;
    let gameplay_dropped = admin
        .run(&format!("DROP DATABASE IF EXISTS {gameplay_db}"))
        .await;

    let report = result?;
    boot_dropped?;
    gameplay_dropped?;
    Ok(report)
}

async fn run_inner(
    conn: &ConnArgs,
    args: &Args<'_>,
    boot_db: &str,
    gameplay_db: &str,
) -> Result<Report, CanonicalError> {
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

    eprintln!("# --- setting up boot window (fresh reset state) ---");
    create_and_load_database(
        &db_at(conn, "default"),
        conn,
        boot_db,
        args.bin,
        args.manifest_path,
        text_start_word,
        text_end_word,
    )
    .await?;
    {
        let boot_conn_db = db_at(conn, boot_db);
        crate::bootstrap::seed(&boot_conn_db, &crate::bootstrap::RESET_REGS).await?;
    }

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

    eprintln!("# --- setting up gameplay window (seeded from snapshot) ---");
    create_and_load_database(
        &db_at(conn, "default"),
        conn,
        gameplay_db,
        args.bin,
        args.manifest_path,
        text_start_word,
        text_end_word,
    )
    .await?;
    {
        let gameplay_conn_db = db_at(conn, gameplay_db);
        gameplay_conn_db.run("TRUNCATE TABLE ram").await?;
        seed_snapshot(&gameplay_conn_db, &snapshot).await?;
    }

    let windows = run_both_windows(
        conn,
        args,
        boot_db,
        gameplay_db,
        &snapshot,
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
    )
    .await?;

    let decoded_rows: u64 = db_at(conn, boot_db)
        .fetch_one("SELECT count(DISTINCT word_addr) FROM decoded")
        .await
        .unwrap_or(0);

    Ok(Report {
        rom_sha256,
        decoded_rows,
        k: args.k,
        hwm: args.hwm,
        batches: args.batches,
        git_sha,
        windows,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_both_windows(
    conn: &ConnArgs,
    args: &Args<'_>,
    boot_db: &str,
    gameplay_db: &str,
    snapshot: &Snapshot,
    text_start_widx: u32,
    text_end_widx: u32,
    decn: u32,
    ram_words: u32,
) -> Result<Vec<WindowResult>, CanonicalError> {
    let boot_conn_db = db_at(conn, boot_db);
    let boot_regs0 = wrap_regs(&[0u32; 31]);
    let boot_label = format!("boot: [0, {})", args.windows.boot_end_icount);
    let boot_shape = Shape {
        k: args.k,
        hwm: args.hwm,
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        database: boot_db,
    };
    let boot = run_window(
        &boot_conn_db,
        &boot_label,
        &boot_shape,
        args.batches,
        RAM_BASE,
        &boot_regs0,
    )
    .await?;

    let gameplay_conn_db = db_at(conn, gameplay_db);
    let snapshot_pc = snapshot.header.pc.expect("machine snapshot carries pc");
    let snapshot_regs = snapshot
        .header
        .regs
        .as_ref()
        .expect("machine snapshot carries regs")[1..32]
        .to_vec();
    let gameplay_regs0 = wrap_regs(&snapshot_regs);
    let gameplay_label = format!(
        "store-heavy gameplay: [{}, {})",
        args.windows.gameplay_target_icount, args.windows.gameplay_window_end_icount
    );
    let gameplay_shape = Shape {
        k: args.k,
        hwm: args.hwm,
        text_start_widx,
        text_end_widx,
        decn,
        ram_words,
        database: gameplay_db,
    };
    let gameplay = run_window(
        &gameplay_conn_db,
        &gameplay_label,
        &gameplay_shape,
        args.batches,
        snapshot_pc,
        &gameplay_regs0,
    )
    .await?;

    Ok(vec![boot, gameplay])
}

pub(crate) fn db_at(conn: &ConnArgs, database: &str) -> Db {
    let mut c = conn.clone();
    c.database = database.to_string();
    c.connect()
}
