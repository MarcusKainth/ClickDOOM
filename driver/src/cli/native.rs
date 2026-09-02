//! The `native` namespace.
//!
//! Every subcommand here shares one connection and
//! [`ConnArgs`](crate::client::ConnArgs).

use std::time::{Duration, Instant}; // purity-ok: pacing and latency measurement in the driver, never a value a statement reads

use bytes::Bytes;
use clap::{Args, Subcommand};

use super::{Exit, Failure, failed, gate};
use crate::client::{ConnArgs, Db};
use crate::native::rowbinary::Row;
use crate::native::settings::resident_settings;
use crate::native::stream::Resident;

/// `clickdoom native`.
#[derive(Args)]
#[command(
    about = "DOOM's own simulation and renderer, as SQL",
    // Hard-wrapped: clap only rewraps help text with its `wrap_help`
    // feature, which this binary does not enable.
    long_about = "\
DOOM's own simulation and renderer, expressed as ClickHouse SQL: the tic
loop, the game state and the frame, with no instruction-set emulator
underneath. native/README.md states what the SQL has to do, and
clickdoom_spec::native_state fixes the state it carries between tics.

`clickdoom emulation --help` lists what runs the DOOM ROM on the CPU in SQL."
)]
pub struct NativeCmd {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    // Described by `SessionCheckCmd`'s own `about` and `long_about`. A doc
    // comment here would replace both with its first line.
    SessionCheck(SessionCheckCmd),
}

/// The schema the check's statement reads.
const CHECK_INPUT_SCHEMA: &str = "tic UInt32, pad String";

/// One DOOM tic at 35 Hz, the rate the check paces itself to.
const TIC: Duration = Duration::from_micros(28_571);

/// How long one row may take to appear before the check gives up.
const VISIBLE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Args)]
#[command(
    about = "Measure what a resident statement costs on a given server",
    long_about = "\
Stream rows into one INSERT ... SELECT ... FROM input(...) held open for the
whole run, reading each row back before sending the next, and report the
send-to-visible latency. This is the transport native mode runs on, measured
against the server you point it at.

The check writes to its own throwaway table and drops it again. Nothing else
in the database is read or touched.

Exit codes: 0 the transport works and is fast enough, 1 it did not run, 3 it
ran and the median was over --max-p50-ms."
)]
pub struct SessionCheckCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Rows to stream, one per tic
    #[arg(long, default_value_t = 100)]
    pub rows: u32,
    /// Fail with exit 3 if the median send-to-visible is over this
    #[arg(long, default_value_t = 5.0, value_name = "MS")]
    pub max_p50_ms: f64,
}

pub(super) async fn run(cmd: &NativeCmd) -> Result<Exit, Failure> {
    match &cmd.command {
        Command::SessionCheck(cmd) => session_check(cmd).await,
    }
}

/// What the check measured.
struct Measured {
    p50: Duration,
    p99: Duration,
    max: Duration,
}

async fn session_check(cmd: &SessionCheckCmd) -> Result<Exit, Failure> {
    if cmd.rows == 0 {
        return Err(Failure {
            exit: Exit::Usage,
            message: "--rows has to be at least 1".into(),
        });
    }
    let db = cmd.conn.connect_uncompressed();
    let version = db
        .fetch_one::<String>("SELECT version()")
        .await
        .map_err(|err| {
            failed(format!(
                "cannot reach ClickHouse at {}:{} as {}: {err}. \
                 Check --host, --port, --user and the password, \
                 and that the server is running",
                cmd.conn.host, cmd.conn.port, cmd.conn.user
            ))
        })?;
    let table = format!("{}.session_check_{}", cmd.conn.database, std::process::id());

    create_table(&db, &table).await?;
    let measured = stream_rows(cmd, &db, &table).await;
    // The table goes whether the run worked or not, so a failed check does
    // not leave one behind for the next.
    let dropped = db.run(&format!("DROP TABLE IF EXISTS {table}")).await;
    let measured = measured?;
    dropped.map_err(|err| failed(format!("dropping {table}: {err}")))?;

    println!(
        "ClickHouse {version}: {} rows at 35 Hz, \
         send-to-visible p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        cmd.rows,
        millis(measured.p50),
        millis(measured.p99),
        millis(measured.max)
    );
    if millis(measured.p50) > cmd.max_p50_ms {
        return Err(gate(format!(
            "median send-to-visible {:.2} ms is over the {:.2} ms --max-p50-ms allows. \
             A tic's budget at 35 Hz is 28.6 ms, so a slow transport leaves the \
             simulation and the renderer nothing",
            millis(measured.p50),
            cmd.max_p50_ms
        )));
    }
    Ok(Exit::Ok)
}

async fn create_table(db: &Db, table: &str) -> Result<(), Failure> {
    let sql =
        format!("CREATE TABLE {table} (tic UInt32, doubled UInt32) ENGINE = Join(ANY, LEFT, tic)");
    db.run(&sql)
        .await
        .map_err(|err| failed(format!("creating {table}: {err}")))
}

/// Opens the statement, streams `cmd.rows` rows through it and times each
/// one from the send to the moment it can be read back.
async fn stream_rows(cmd: &SessionCheckCmd, db: &Db, table: &str) -> Result<Measured, Failure> {
    let statement = format!(
        "INSERT INTO {table} SELECT tic, tic * 2 FROM input('{CHECK_INPUT_SCHEMA}') WHERE tic > 0"
    );
    let resident = Resident::open(
        &cmd.conn,
        &statement,
        CHECK_INPUT_SCHEMA,
        &resident_settings(statement.len()),
    )
    .await
    .map_err(|err| failed(format!("opening the resident statement: {err}")))?;

    let mut visible = Vec::with_capacity(cmd.rows as usize);
    for tic in 1..=cmd.rows {
        let took = send_and_wait(&resident, db, table, tic).await?;
        visible.push(took);
        if let Some(rest) = TIC.checked_sub(took) {
            tokio::time::sleep(rest).await;
        }
    }
    resident
        .close()
        .await
        .map_err(|err| failed(format!("the statement did not finish cleanly: {err}")))?;

    visible.sort_unstable();
    Ok(Measured {
        p50: percentile(&visible, 0.50),
        p99: percentile(&visible, 0.99),
        max: visible[visible.len() - 1],
    })
}

async fn send_and_wait(
    resident: &Resident,
    db: &Db,
    table: &str,
    tic: u32,
) -> Result<Duration, Failure> {
    let sent = Instant::now(); // purity-ok: measuring send-to-visible, see the import
    resident
        .send(row(tic))
        .map_err(|err| failed(format!("sending row {tic}: {err}")))?;
    let query = format!("SELECT joinGet('{table}', 'doubled', toUInt32({tic}))");
    loop {
        let doubled = db
            .fetch_one::<u32>(&query)
            .await
            .map_err(|err| failed(format!("reading row {tic} back: {err}")))?;
        if doubled == tic * 2 {
            return Ok(sent.elapsed());
        }
        if sent.elapsed() >= VISIBLE_TIMEOUT {
            return Err(failed(format!(
                "row {tic} was still not readable after {VISIBLE_TIMEOUT:?}. The statement \
                 has stopped; look for its exception in system.query_log"
            )));
        }
    }
}

fn row(tic: u32) -> Bytes {
    let mut row = Row::with_capacity(8);
    row.u32(tic).bytes(b"");
    row.finish()
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    let last = sorted.len().saturating_sub(1) as f64;
    sorted[(last * fraction).round() as usize]
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}
