//! Timing one tic of the simulation's first statement against a world that
//! does not move.
//!
//! What a stage costs is measured by building the statement without it and
//! taking the difference, which
//! [`clickdoom_native::sql::sim::tick::bench`] builds. The measurement only
//! means anything if every cut runs the same tic over the same world: a cut
//! leaves the columns its stage would have written at the tic before, so a
//! run that let each cut carry on would have each of them simulating
//! something different within a few tics, and their costs would not be
//! comparable.
//!
//! So this advances the simulation to the tic before the one under
//! measurement, once, with the statements as they ship in a resident
//! session, and then runs each cut over that single tic. Every cut reads
//! the same `native_state` row and does the same tic's work.

use std::time::Duration; // purity-ok: carrying what the server reported it spent, never a value a statement reads

use clickdoom_native::sql::sim::tick::{Input, bench};
use clickhouse::Row;
use serde::Deserialize;

use super::db::Fixture;
use super::resident;

/// How many times the tic runs in the longer of the two statements. The
/// marginal cost is the slope between one and this.
const REPEATS: u32 = 11;

/// What one cut cost.
#[derive(Debug, Clone)]
pub struct Cost {
    /// The cut, or `None` for the statement as it ships.
    pub cut: Option<bench::Cut>,
    /// The analysis, which a session pays once however many tics follow.
    pub analysed: Duration,
    /// The server's own time for a statement running the tic once,
    /// analysis included.
    pub once: Duration,
    /// What one more tic costs, as the slope between a statement running
    /// the tic once and one running it [`REPEATS`] times.
    ///
    /// This is the number a per-tic table wants. The difference cancels
    /// everything a statement pays once whatever follows: the analysis,
    /// and the scalar constants the map tables are read into, which a
    /// resident session also pays once and then reuses for every tic.
    /// Subtracting the analysis from a single-tic run instead charges that
    /// setup to the one tic carrying it, which is why such a figure reads
    /// as seconds where a session's tic reads as milliseconds.
    pub marginal: Duration,
}

impl Cost {
    /// The name the generator knows this cut by, or `full`.
    pub fn name(&self) -> &'static str {
        self.cut.map_or("full", bench::Cut::name)
    }
}

#[derive(Row, Deserialize)]
struct Walked {
    rows: u64,
    leveltime: i32,
    start: i32,
    things: u64,
}

/// Panics unless `native_state` holds a row at `tic` that the simulation
/// walked to from the level's own row at tic 0: one row, `tic` tics of
/// `leveltime` past tic 0's, and the level's things still on the list.
///
/// A tic whose first statement read a `native_state` row that did not
/// exist yet reads every column at its default, so its row starts
/// `leveltime` over and carries no things.
async fn assert_walked(fixture: &Fixture, tic: u32) {
    let db = &fixture.database;
    let walked: Walked = fixture
        .scalar(&format!(
            "SELECT toUInt64(count()) AS rows, \
             toInt32(any(leveltime)) AS leveltime, \
             toInt32(joinGet('{db}.native_state', 'leveltime', toUInt32(0))) AS start, \
             toUInt64(any(length(m_x))) AS things \
             FROM {db}.native_state WHERE tic = {tic}"
        ))
        .await;
    assert_eq!(walked.rows, 1, "no state row at tic {tic}");
    assert!(
        walked.leveltime == walked.start + tic as i32 && walked.things > 0,
        "the state row at tic {tic} is not one the simulation walked to: \
         leveltime {} from {} at tic 0, {} things",
        walked.leveltime,
        walked.start,
        walked.things
    );
}

#[derive(Row, Deserialize)]
struct Timing {
    duration_ms: u64,
    analysis_us: u64,
}

/// Runs the simulation forward to the tic before `tic` through the resident
/// session, checks the row it left is a walked one, then times each of
/// `cuts` over `tic` alone.
///
/// The caller has already loaded the level. `tic` is one-based the way the
/// session counts, so the smallest useful one is 2: tic 1 has no earlier
/// row to read and pays for it.
pub async fn at_tic(fixture: &Fixture, tic: u32, cuts: &[Option<bench::Cut>]) -> Vec<Cost> {
    assert!(tic > 1, "tic {tic} has no earlier state row to read");
    let ahead: Vec<Input> = (1..tic).map(Input::demo).collect();
    resident::run(fixture, &ahead, false).await;
    assert_walked(fixture, tic - 1).await;

    let mut costs = Vec::new();
    for cut in cuts {
        let once = run_rows(fixture, tic, *cut, 1).await;
        let repeated = run_rows(fixture, tic, *cut, REPEATS).await;
        costs.push(Cost {
            cut: *cut,
            analysed: once.analysed,
            once: once.ran,
            marginal: repeated
                .ran
                .saturating_sub(once.ran)
                .checked_div(REPEATS - 1)
                .unwrap_or_default(),
        });
    }
    costs
}

/// Runs `tic` through `cut` `rows` times in one statement, and reports what
/// the server spent on it.
///
/// Every row is the same tic, so each reads the same `native_state` row and
/// does the same work. `native_stage` is a `Join(ANY, LEFT, tic)` under
/// `join_any_take_last_row`, so each repeat replaces the tic's row rather
/// than adding one: the table stays one row per tic however many times a
/// tic runs, and the world the next cut reads is the one this cut read.
async fn run_rows(fixture: &Fixture, tic: u32, cut: Option<bench::Cut>, rows: u32) -> Timed {
    let feed: Vec<Input> = (0..rows).map(|_| Input::demo(tic)).collect();
    let statement = bench::stage1(&fixture.database, cut, &feed);
    fixture
        .execute(std::slice::from_ref(&statement))
        .await
        .unwrap_or_else(|e| {
            panic!(
                "cut {} at tic {tic} over {rows} rows: {e}",
                cut.map_or("full", bench::Cut::name)
            )
        });
    last_timing(fixture).await
}

/// One statement's own time, as the server recorded it.
struct Timed {
    ran: Duration,
    analysed: Duration,
}

/// What the server recorded for the statement just issued. Every fixture
/// names its own database, so the last `native_stage` insert under that
/// name is this call's own.
async fn last_timing(fixture: &Fixture) -> Timed {
    fixture
        .execute(&[clickdoom_native::sql::Statement::sql("SYSTEM FLUSH LOGS")])
        .await
        .expect("flushing the query log");
    let timing: Timing = fixture
        .scalar(&format!(
            "SELECT toUInt64(query_duration_ms) AS duration_ms, \
             toUInt64(ProfileEvents['QueryAnalysisMicroseconds']) AS analysis_us \
             FROM system.query_log \
             WHERE type = 'QueryFinish' \
             AND query LIKE 'INSERT INTO {}.native_stage%' \
             ORDER BY event_time_microseconds DESC LIMIT 1",
            fixture.database
        ))
        .await;
    Timed {
        ran: Duration::from_millis(timing.duration_ms),
        analysed: Duration::from_micros(timing.analysis_us),
    }
}
