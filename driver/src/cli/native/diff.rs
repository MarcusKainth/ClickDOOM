//! `clickdoom native diff`: the simulation against the reference emulator,
//! tic by tic.

use std::path::PathBuf;
use std::time::Duration; // purity-ok: the tic budget and the timings the session measured, read from no clock here

use clap::Args;
use clickdoom_native::sql::sim::tick;
use clickdoom_native::sql::{self, Statement, parity};
use clickhouse::Row;
use serde::Deserialize;

use crate::cli::{Exit, Failure, failed, gate};
use crate::client::{ConnArgs, Db};
use crate::native::session::TIC_TIMEOUT;
use crate::native::{Session, plan, probe};
use crate::stats::{Clock, Monotonic};

/// How often the progress line comes out.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);

/// How long the first tic may take, which is the statement being analysed.
const FIRST_TIC_TIMEOUT: Duration = Duration::from_secs(300);

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

Exit codes: 0 the two agree over TICS tics, 1 the run failed, 3 they
diverged."
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

    let session = Session::open(
        &cmd.conn,
        database,
        Some(&tick::resident_statement(database)),
        None,
    )
    .await
    .map_err(|err| failed(format!("opening the simulation: {err}")))?;
    let ran = simulate(&session, cmd.tics).await;
    let closed = session.close().await;
    match (ran, closed) {
        (Ok(()), Ok(())) => {}
        (_, Err(err)) => return Err(failed(format!("the simulation statement failed: {err}"))),
        (Err(failure), Ok(())) => return Err(failure),
    }

    report(cmd, &db).await
}

/// Empties `native_state` and writes the level's first row again.
///
/// The comparison covers every tic both tables hold, so a run that left its
/// own rows behind would have them compared by the next one. A diff run
/// starts from the level as it stands at tic 0, whatever ran before it.
async fn restart(db: &Db, database: &str) -> Result<(), Failure> {
    let phases = [
        plan::Phase::new(
            "empty",
            vec![Statement::sql(format!(
                "TRUNCATE TABLE IF EXISTS {database}.{}",
                probe::STATE_TABLE
            ))],
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

/// Runs tic 1 to `tics`, one row at a time, as an interactive run does.
async fn simulate(session: &Session, tics: u32) -> Result<(), Failure> {
    let clock = Monotonic::new();
    let mut last = Duration::ZERO;
    for tic in 1..=tics {
        session
            .feed_sim(tic, tick::source::DEMO, 0, 0, 0)
            .map_err(|err| failed(format!("feeding tic {tic}: {err}")))?;
        session
            .wait_sim(tic, timeout(tic))
            .await
            .map_err(|err| failed(err.to_string()))?;
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
    Ok(())
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
async fn report(cmd: &DiffCmd, db: &Db) -> Result<Exit, Failure> {
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
    let Some(first) = first.first() else {
        println!("no divergence: every field agrees over the {compared} tics both sides hold");
        return Ok(Exit::Ok);
    };
    Err(gate(format!(
        "tic {} {} slot {} {}: {} against the probe's {}",
        first.tic, first.kind, first.slot, first.field, first.ours, first.theirs
    )))
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
