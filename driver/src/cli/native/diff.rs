//! `clickdoom native diff`: the simulation against the reference emulator,
//! tic by tic.

use std::path::PathBuf;
use std::time::Duration; // purity-ok: the tic budget and the timings the session measured, read from no clock here

use clap::Args;
use clickdoom_native::resident::{FIRST_TIC_TIMEOUT, TIC_TIMEOUT};
use clickdoom_native::sql::sim::tick;
use clickdoom_native::sql::{self, Statement, parity};
use clickhouse::Row;
use serde::Deserialize;

use crate::cli::{Exit, Failure, failed, gate};
use crate::client::{ConnArgs, Db};
use crate::native::session::STAGE_TABLE;
use crate::native::{Refusal, Session, plan, probe, record, refusal, schema};
use crate::stats::{Clock, Monotonic};

/// How often the progress line comes out.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// How long `--record` waits for the simulation statements' own rows to
/// reach `system.query_log`, and how often it looks.
const QUERY_LOG_TIMEOUT: Duration = Duration::from_secs(30);
const QUERY_LOG_POLL: Duration = Duration::from_millis(250);

#[derive(Args)]
#[command(
    about = "Run the simulation against the reference emulator's own state rows",
    // Hard-wrapped: clap only rewraps help text with its `wrap_help`
    // feature, which this binary does not enable.
    long_about = "\
Load the probe's state rows, run the simulation for TICS tics through the
same session an interactive run uses, and report the first field that
differs.

The probe rows go into probe_state and nowhere else. Copying them into
native_state is what a rendering run does, and doing it here would compare
the run against itself.

The tic commands come from the demo lump, so the run is the one the probe
recorded. --summary also lists every field that ever differs, with the tic
each first did.

A tic native_state marks unresolved or unimplemented is checked before any
field is, since a tic the statement could not produce exactly is not one
to compare.

--record appends one JSON line to PATH with what the run found: the first
refused tic, the first field that differs, the last tic compared, what each
simulation statement's analysis took, the tic time's median and 95th
percentile, the commit, the server version and the CPU model. A run that
fails writes nothing.

Exit codes: 0 the two agree over TICS tics, 1 the run failed, 3 a tic
refused or the two diverged."
)]
pub struct DiffCmd {
    /// Tics to run and compare
    #[arg(value_parser = clap::value_parser!(u32).range(1..))]
    pub tics: u32,
    #[command(flatten)]
    pub conn: ConnArgs,
    /// The reference emulator's probe rows
    #[arg(long, value_name = "PATH")]
    pub probe: PathBuf,
    /// Also list every field that ever differs
    #[arg(long)]
    pub summary: bool,
    /// Append what the run found to this file as one JSON line
    #[arg(long, value_name = "PATH")]
    pub record: Option<PathBuf>,
}

/// What the field comparison found.
struct Compared {
    /// Tics both sides hold, up to the last one the run fed.
    tics: u64,
    first: Option<Divergence>,
}

/// One row of `parity::first_divergence`.
#[derive(Row, Deserialize)]
struct Divergence {
    tic: u32,
    kind: String,
    slot: u32,
    field: String,
    ours: String,
    theirs: String,
}

/// One row of `parity::field_summary`.
#[derive(Row, Deserialize)]
struct FieldRow {
    field: String,
    kind: String,
    tics: u64,
    first_tic: u32,
    slot: u32,
    ours: String,
    theirs: String,
}

pub(crate) async fn run(cmd: &DiffCmd) -> Result<Exit, Failure> {
    let database = &cmd.conn.database;
    let db = cmd.conn.connect();
    if let Some(mismatch) = schema::check_hash(&db, database)
        .await
        .map_err(|err| failed(err.to_string()))?
    {
        return Err(failed(mismatch.to_string()));
    }

    restart(&db, database).await?;
    let staged = probe::stage(&db, database, &cmd.probe)
        .await
        .map_err(|err| failed(err.to_string()))?;
    println!(
        "{}: {} rows over {} tics into {database}.{}",
        cmd.probe.display(),
        staged.rows,
        staged.tics,
        probe::STAGING_TABLE
    );

    let (stage1, stage2) = tick::resident_statements(database);
    let session = Session::open(&cmd.conn, database, Some((&stage1, &stage2)), None)
        .await
        .map_err(|err| failed(format!("opening the simulation: {err}")))?;
    let query_ids = [
        session.sim_query_id().to_owned(),
        session.sim2_query_id().to_owned(),
    ];
    let ran = simulate(&session, cmd.tics).await;
    let closed = session.close().await;
    let elapsed = match (ran, closed) {
        (Ok(elapsed), Ok(())) => elapsed,
        (_, Err(err)) => return Err(failed(format!("the simulation statement failed: {err}"))),
        (Err(failure), Ok(())) => return Err(failure),
    };

    // A refused tic is checked before the fields are, because a tic the
    // statement itself could not produce is not one to compare field by
    // field against the probe.
    let refused = refusal::first(&db, database, cmd.tics)
        .await
        .map_err(|err| failed(format!("reading whether a tic refused: {err}")))?;
    let compared = match refused {
        Some(_) => None,
        None => Some(compare(cmd, &db).await?),
    };

    if let Some(path) = &cmd.record {
        let found = found(
            cmd,
            &db,
            &query_ids,
            &elapsed,
            refused.as_ref(),
            compared.as_ref(),
        )
        .await?;
        record::append(path, &found).map_err(|err| failed(err.to_string()))?;
    }

    match (refused, compared) {
        (Some(refusal), _) => Err(gate(refusal.to_string())),
        (None, Some(compared)) => report(compared),
        (None, None) => unreachable!("a run that did not refuse is compared"),
    }
}

/// Empties `native_state` and `native_stage` and writes the level's first
/// row again.
///
/// The comparison covers every tic both tables hold, so a run that left its
/// own rows behind would have them compared by the next one. A diff run
/// starts from the level as it stands at tic 0, whatever ran before it.
/// `native_stage` empties the same way: left behind, it holds a row a prior
/// run staged for a tic this run has not reached yet, and this run's own
/// presence check for that tic would find it before this run's own first
/// statement has written it.
async fn restart(db: &Db, database: &str) -> Result<(), Failure> {
    let phases = [
        plan::Phase::new(
            "empty",
            vec![
                Statement::sql(format!(
                    "TRUNCATE TABLE IF EXISTS {database}.{}",
                    probe::STATE_TABLE
                )),
                Statement::sql(format!("TRUNCATE TABLE IF EXISTS {database}.{STAGE_TABLE}")),
            ],
        ),
        plan::Phase::new("sim", sql::sim::load_statements(database)),
    ];
    plan::run(db, &phases)
        .await
        .map(|_| ())
        .map_err(|err| failed(err.to_string()))
}

/// What one tic may take. The first pays for the statement's analysis,
/// which is seconds; every one after it is milliseconds.
fn timeout(tic: u32) -> Duration {
    match tic {
        1 => FIRST_TIC_TIMEOUT,
        _ => TIC_TIMEOUT,
    }
}

/// Runs tic 1 to `tics`, one row at a time, as an interactive run does,
/// and returns what each tic took.
async fn simulate(session: &Session, tics: u32) -> Result<Vec<Duration>, Failure> {
    let clock = Monotonic::new();
    let mut last = Duration::ZERO;
    let mut elapsed = Vec::with_capacity(tics as usize);
    for tic in 1..=tics {
        session
            .feed_sim(tic, tick::source::DEMO, 0, 0, 0)
            .map_err(|err| failed(format!("feeding tic {tic}: {err}")))?;
        let ran = session
            .wait_sim(tic, timeout(tic))
            .await
            .map_err(|err| failed(err.to_string()))?;
        elapsed.push(ran.elapsed);
        let now = clock.elapsed();
        if now.saturating_sub(last) >= PROGRESS_INTERVAL {
            last = now;
            eprintln!(
                "# native diff elapsed={:.1}s tics={tic} tics/s={:.1}",
                now.as_secs_f64(),
                f64::from(tic) / now.as_secs_f64()
            );
        }
    }
    Ok(elapsed)
}

/// How many tics the comparison actually covers: the ones the run produced
/// that the probe also recorded.
///
/// A run whose probe covers none of its tics finds no divergence, which
/// reads exactly like agreement. The count is what tells the two apart.
async fn compared(cmd: &DiffCmd, db: &Db) -> Result<u64, Failure> {
    let database = &cmd.conn.database;
    db.fetch_one::<u64>(&format!(
        "SELECT uniqExact(gametic) FROM {database}.{} \
         WHERE gametic <= {} AND gametic IN (SELECT tic FROM {database}.{})",
        probe::STAGING_TABLE,
        cmd.tics,
        probe::STATE_TABLE
    ))
    .await
    .map_err(|err| failed(format!("counting the tics both sides hold: {err}")))
}

/// The comparison itself, which is one query per question.
async fn compare(cmd: &DiffCmd, db: &Db) -> Result<Compared, Failure> {
    let database = &cmd.conn.database;
    let compared = compared(cmd, db).await?;
    if compared == 0 {
        return Err(failed(format!(
            "the probe records none of the {} tics this ran, so nothing was \
             compared. Run more tics, or point --probe at a file that covers \
             these",
            cmd.tics
        )));
    }
    if cmd.summary {
        let fields: Vec<FieldRow> = db
            .fetch_all(&parity::field_summary(database))
            .await
            .map_err(|err| failed(format!("reading the field summary: {err}")))?;
        for row in &fields {
            println!(
                "{:<24} {:<6} slot={:<4} tics={:<6} first_tic={:<6} ours={} theirs={}",
                row.field, row.kind, row.slot, row.tics, row.first_tic, row.ours, row.theirs
            );
        }
    }

    let first: Vec<Divergence> = db
        .fetch_all(&parity::first_divergence(database))
        .await
        .map_err(|err| failed(format!("reading the first divergence: {err}")))?;
    Ok(Compared {
        tics: compared,
        first: first.into_iter().next(),
    })
}

/// Prints what the comparison found and turns it into the exit code.
fn report(compared: Compared) -> Result<Exit, Failure> {
    let Some(first) = compared.first else {
        println!(
            "no divergence: every field agrees over the {} tics both sides hold",
            compared.tics
        );
        return Ok(Exit::Ok);
    };
    Err(gate(format!(
        "tic {} {}: {} against the probe's {}",
        first.tic,
        first.location(),
        first.ours,
        first.theirs
    )))
}

impl Divergence {
    /// The row and the field, as `mobj slot 1 m_momx`.
    fn location(&self) -> String {
        format!("{} slot {} {}", self.kind, self.slot, self.field)
    }
}

/// The line `--record` appends for this run.
///
/// The analysis times are read from `system.query_log` by the two
/// simulation statements' own query ids, after the session has closed them.
async fn found(
    cmd: &DiffCmd,
    db: &Db,
    query_ids: &[String; 2],
    elapsed: &[Duration],
    refused: Option<&Refusal>,
    compared: Option<&Compared>,
) -> Result<record::Record, Failure> {
    let clickhouse = db
        .fetch_one::<String>("SELECT version()")
        .await
        .map_err(|err| failed(format!("reading the server version: {err}")))?;
    let [stage1, stage2] = analysis(db, query_ids).await?;
    let divergence = compared.and_then(|compared| compared.first.as_ref());
    // The first tic pays for both statements' analysis, which is recorded
    // on its own.
    let mut warm: Vec<Duration> = elapsed.iter().skip(1).copied().collect();
    warm.sort_unstable();
    let tic_ms =
        |fraction| (!warm.is_empty()).then(|| super::millis(super::percentile(&warm, fraction)));
    Ok(record::Record {
        commit: record::commit().unwrap_or_default(),
        clickhouse: Some(clickhouse),
        runner_cpu: record::cpu_model(),
        first_refused_tic: refused.map(|refusal| refusal.tic),
        first_refused_bits: refused.map(|refusal| refusal.reason.clone()),
        first_divergent_tic: divergence.map(|first| first.tic),
        first_divergent_field: divergence.map(Divergence::location),
        compared_through: compared.map(|_| cmd.tics),
        compared_tics: compared.map(|compared| compared.tics),
        stage1_analysis_s: Some(stage1),
        stage2_analysis_s: Some(stage2),
        tic_ms_p50: tic_ms(0.50),
        tic_ms_p95: tic_ms(0.95),
        tics: Some(cmd.tics),
        run_id: record::run_id(),
        error: None,
    })
}

/// `QueryAnalysisMicroseconds` of each of `query_ids`, in seconds.
///
/// The server logs a statement's finish after it has answered the close,
/// so the log is flushed and read again until both rows are there or
/// [`QUERY_LOG_TIMEOUT`] has passed.
async fn analysis(db: &Db, query_ids: &[String; 2]) -> Result<[f64; 2], Failure> {
    let sql = format!(
        "SELECT query_id, toUInt64(ProfileEvents['QueryAnalysisMicroseconds']) \
         FROM system.query_log \
         WHERE type = 'QueryFinish' AND query_id IN ('{}', '{}')",
        query_ids[0], query_ids[1]
    );
    let clock = Monotonic::new();
    loop {
        db.run("SYSTEM FLUSH LOGS")
            .await
            .map_err(|err| failed(format!("flushing system.query_log: {err}")))?;
        let rows: Vec<(String, u64)> = db
            .fetch_all(&sql)
            .await
            .map_err(|err| failed(format!("reading system.query_log: {err}")))?;
        let seconds = |id: &String| {
            rows.iter()
                .find(|(query_id, _)| query_id == id)
                .map(|(_, micros)| *micros as f64 / 1e6)
        };
        match (seconds(&query_ids[0]), seconds(&query_ids[1])) {
            (Some(stage1), Some(stage2)) => return Ok([stage1, stage2]),
            _ if clock.elapsed() >= QUERY_LOG_TIMEOUT => {
                return Err(failed(format!(
                    "system.query_log holds no finished row for the simulation \
                     statements {} and {} after {QUERY_LOG_TIMEOUT:?}, so their \
                     analysis time is unknown",
                    query_ids[0], query_ids[1]
                )));
            }
            _ => tokio::time::sleep(QUERY_LOG_POLL).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Only {
        #[command(flatten)]
        diff: DiffCmd,
    }

    fn parsed(args: &[&str]) -> DiffCmd {
        let mut all = vec!["diff"];
        all.extend_from_slice(args);
        Only::try_parse_from(all).expect("the arguments parse").diff
    }

    #[test]
    fn the_tic_count_is_positional_and_the_probe_is_required() {
        assert!(Only::try_parse_from(["diff", "100"]).is_err());
        let cmd = parsed(&["100", "--probe", "p.tsv"]);
        assert_eq!(cmd.tics, 100);
        assert_eq!(cmd.probe, PathBuf::from("p.tsv"));
        assert!(!cmd.summary);
        assert_eq!(cmd.record, None);
        let cmd = parsed(&["100", "--probe", "p.tsv", "--record", "r.jsonl"]);
        assert_eq!(cmd.record, Some(PathBuf::from("r.jsonl")));
    }

    /// The comparison needs a tic to compare, and running none of them and
    /// reporting agreement would be a check that never ran.
    #[test]
    fn running_no_tics_does_not_parse() {
        let Err(error) = Only::try_parse_from(["diff", "0", "--probe", "p.tsv"]) else {
            panic!("zero tics compares nothing and must not parse");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(Only::try_parse_from(["diff", "1", "--probe", "p.tsv"]).is_ok());
    }
}
