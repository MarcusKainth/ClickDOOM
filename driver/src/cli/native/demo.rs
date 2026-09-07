//! `clickdoom native demo`: a demo played at 35 Hz, in a window.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration; // purity-ok: the frame budget and the timings the session measured, read from no clock here

use clap::{Args, ValueEnum};
use clickdoom_native::resident::{FIRST_TIC_TIMEOUT, TIC_TIMEOUT};
use clickdoom_native::sql::sim::tick;
use tokio::sync::mpsc;

use crate::cli::{Exit, Failure, failed, gate};
use crate::client::{ConnArgs, Db};
use crate::native::pace::{Pace, TIC};
use crate::native::schedule::MeltFrame;
use crate::native::session::{STAGE_TABLE, STATE_TABLE, SessionError};
use crate::native::window::{Scale, Window};
use crate::native::{Refusal, Session, plan, schedule, schema};
use crate::render::{FB_HEIGHT, FB_WIDTH, ppm_sql_over};
use crate::stats::{Clock, Monotonic, NativeCounters, NativeStatsLine};

/// How long one frame may take before the run calls the renderer dead. The
/// first frame of a session pays for the statement's analysis and its
/// scalar constants, which is seconds; every frame after it is milliseconds.
const FRAME_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the progress line comes out.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

/// Where a run's state rows come from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Source {
    /// Rows the reference emulator's probe recorded, loaded by
    /// `clickdoom native load --probe`
    Probe,
    /// Rows this run's own simulation writes, from the demo lump's commands
    Sim,
}

#[derive(Args)]
#[command(
    about = "Play a demo at 35 Hz, in a window",
    // Hard-wrapped: clap only rewraps help text with its `wrap_help`
    // feature, which this binary does not enable.
    long_about = "\
Feed the renderer one frame per tic on a fixed 35 Hz clock and put each one
on the screen as SQL produced it. A frame that overruns costs its own
lateness and nothing more: the deadline moves on by one tic whatever
happened, so there is never a catch-up burst and no frame is skipped. The
progress line reports the rate the last second actually achieved.

--from probe draws from the rows `clickdoom native load --probe` put in the
database, and which tic each frame reads and how far the screen melt has
got come from those rows.

--from sim opens the simulation statement too and runs it from the demo
lump's own commands, the way `native diff` does. It empties the database's
simulation rows first and writes the level's first row again, the same
restart `native diff` does, so a run always plays from the level's own
start rather than from whatever an earlier run of either command left
committed. The renderer draws each frame from the tic the simulation just
wrote: the screen melt's frame count and per-frame pass count are the
reference run's own recorded schedule (`driver/melt/`), not something a
tic count derives, so every melt frame reads the tic that schedule holds
and gameplay resumes one tic per frame after it. The simulation runs ahead
of the paced frames by up to --lookahead tics; the stats line's sim= and
lookahead= report it. A run without --stop-at-frame goes until the demo
lump runs out of commands, which ends the run the same as reaching probe's
last frame does.

--no-window runs headless, which is what --frame-dir and --hash-out are for:
a PPM per frame, built in SQL, and a TSV of frame, tic and frame hash. The
PPM is a query of its own per frame and the progress line reports what it
costs, so a run that writes them does not hold 35 Hz. --stop-at-frame ends
the run early; without it the run ends with the demo.

A tic native_state marks unresolved or unimplemented stops the run rather
than going on to the tics after it.

Exit codes: 0 the run finished, 1 it failed, 3 a tic refused, or
--expect-probe-fbhash found a frame the engine did not draw."
)]
pub struct DemoCmd {
    /// The demo to play, by lump name
    #[arg(default_value = "demo3")]
    pub demo: String,
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Where the state rows come from
    #[arg(long, value_enum, default_value_t = Source::Probe)]
    pub from: Source,
    /// How much bigger than 320x200 the window is drawn
    #[arg(long, value_enum, default_value = "2")]
    pub scale: Scale,
    /// Run without a window
    #[arg(long)]
    pub no_window: bool,
    /// Write a binary PPM per frame here, named by the frame
    #[arg(long, value_name = "DIR")]
    pub frame_dir: Option<PathBuf>,
    /// Write `frame tic fb_hash` per frame here
    #[arg(long, value_name = "PATH")]
    pub hash_out: Option<PathBuf>,
    /// Stop once this frame has been drawn
    #[arg(long, value_name = "N")]
    pub stop_at_frame: Option<u32>,
    /// Fail with exit 3 on the first frame the engine did not draw
    #[arg(long)]
    pub expect_probe_fbhash: bool,
    /// Tics the simulation may run ahead of the frame being drawn, for
    /// --from sim
    #[arg(long, value_name = "N", default_value_t = 35)]
    pub lookahead: u32,
}

pub(crate) async fn run(cmd: &DemoCmd) -> Result<Exit, Failure> {
    match cmd.from {
        Source::Probe => run_probe(cmd).await,
        Source::Sim => run_sim(cmd).await,
    }
}

async fn run_probe(cmd: &DemoCmd) -> Result<Exit, Failure> {
    let database = &cmd.conn.database;
    let db = cmd.conn.connect();
    if let Some(mismatch) = schema::check_hash(&db, database)
        .await
        .map_err(|err| failed(err.to_string()))?
    {
        return Err(failed(mismatch.to_string()));
    }
    let plan = schedule::from_probe(&db, database, cmd.stop_at_frame)
        .await
        .map_err(|err| failed(err.to_string()))?;

    let mut out = Output::open(cmd)?;
    schedule::clear_frames(&db, database)
        .await
        .map_err(|err| failed(format!("emptying the frames table: {err}")))?;
    let mut session = Session::open(
        &cmd.conn,
        database,
        None,
        Some(&clickdoom_native::sql::render::frame_transform(database)),
    )
    .await
    .map_err(|err| failed(format!("opening the renderer: {err}")))?;

    println!(
        "{} from {}:{}/{database}: {} frames at {:.1} Hz",
        cmd.demo,
        cmd.conn.host,
        cmd.conn.port,
        plan.len(),
        1.0 / TIC.as_secs_f64()
    );
    let played = play_probe(cmd, &db, &mut session, &plan, &mut out).await;
    let closed = session.close().await;
    finish(out, played, closed)
}

/// The simulation's own commands, run against the demo lump. `--stop-at-frame`
/// bounds it; without one the run goes until the demo runs out of commands.
async fn run_sim(cmd: &DemoCmd) -> Result<Exit, Failure> {
    let database = &cmd.conn.database;
    let db = cmd.conn.connect();
    if let Some(mismatch) = schema::check_hash(&db, database)
        .await
        .map_err(|err| failed(err.to_string()))?
    {
        return Err(failed(mismatch.to_string()));
    }
    // A demo run always plays from the level's own first state row, the
    // way `native diff` does, rather than resuming wherever a previous
    // sim run of this database left off.
    restart_sim(&db, database).await?;
    let melt = schedule::melt_frames(&db, database)
        .await
        .map_err(|err| failed(err.to_string()))?;
    let expect = match cmd.expect_probe_fbhash {
        true => Some(
            schedule::probe_fb_hashes(&db, database)
                .await
                .map_err(|err| failed(err.to_string()))?,
        ),
        false => None,
    };

    let mut out = Output::open(cmd)?;
    schedule::clear_frames(&db, database)
        .await
        .map_err(|err| failed(format!("emptying the frames table: {err}")))?;
    let (stage1, stage2) = tick::resident_statements(database);
    let session = Session::open(
        &cmd.conn,
        database,
        Some((&stage1, &stage2)),
        Some(&clickdoom_native::sql::render::frame_transform(database)),
    )
    .await
    .map_err(|err| failed(format!("opening the session: {err}")))?;

    println!(
        "{} from {}:{}/{database}: the simulation, at {:.1} Hz",
        cmd.demo,
        cmd.conn.host,
        cmd.conn.port,
        1.0 / TIC.as_secs_f64()
    );
    let played = play_sim(cmd, &db, &session, &melt, expect.as_ref(), &mut out).await;
    let closed = session.close().await;
    finish(out, played, closed)
}

/// Empties `native_state` and `native_stage` and writes the level's first
/// row again, the way `native diff`'s own restart does.
///
/// `native_stage` holds a row a prior run staged for a tic this run has not
/// reached yet, and the session's own presence check for that tic would
/// find it before this run's own first statement has written it.
async fn restart_sim(db: &Db, database: &str) -> Result<(), Failure> {
    let phases = [
        plan::Phase::new(
            "empty",
            vec![
                clickdoom_native::sql::Statement::sql(format!(
                    "TRUNCATE TABLE IF EXISTS {database}.{STATE_TABLE}"
                )),
                clickdoom_native::sql::Statement::sql(format!(
                    "TRUNCATE TABLE IF EXISTS {database}.{STAGE_TABLE}"
                )),
            ],
        ),
        plan::Phase::new("sim", clickdoom_native::sql::sim::load_statements(database)),
    ];
    plan::run(db, &phases)
        .await
        .map(|_| ())
        .map_err(|err| failed(err.to_string()))
}

/// What either source ends with: the frames file closed, and the run's
/// outcome and the statements' closing outcome reconciled the same way.
fn finish(
    out: Output,
    played: Result<Played, Failure>,
    closed: Result<(), SessionError>,
) -> Result<Exit, Failure> {
    let played = match (played, closed) {
        (Ok(played), Ok(())) => played,
        (_, Err(err)) => return Err(failed(format!("a statement failed: {err}"))),
        (Err(failure), Ok(())) => return Err(failure),
    };
    out.finish()?;

    if let Some((frame, ours, theirs)) = played.diverged {
        return Err(gate(format!(
            "frame {frame} hashes to {ours}, not the {theirs} the probe \
             recorded. The frame the renderer drew is not the one the engine \
             drew"
        )));
    }
    Ok(Exit::Ok)
}

/// What a run ended with.
struct Played {
    diverged: Option<(u32, String, String)>,
}

/// The paced loop over probed state rows: one frame per tic, on the clock
/// and not on how long the frame took.
///
/// The first frame is drawn before the clock starts. It pays for the
/// statement's analysis and for evaluating its scalar constants, which is
/// seconds, and pacing against it would leave every frame after it late
/// against a deadline that is already minutes in the past.
async fn play_probe(
    cmd: &DemoCmd,
    db: &Db,
    session: &mut Session,
    plan: &[schedule::FrameRow],
    out: &mut Output,
) -> Result<Played, Failure> {
    let clock = Monotonic::new();
    let mut run = Run {
        counters: NativeCounters::default(),
        diverged: None,
    };
    let mut frames = plan.iter();
    let Some(first) = frames.next() else {
        return Ok(Played { diverged: None });
    };
    draw(cmd, db, session, out, first, &clock, &mut run).await?;
    eprintln!(
        "# native ready elapsed={:.1}s render={:.1}ms",
        clock.elapsed().as_secs_f64(),
        run.counters.render.as_secs_f64() * 1e3
    );

    let mut stats = NativeStatsLine::start(Monotonic::new(), STATS_INTERVAL, run.counters);
    let mut pace = Pace::start(TIC, clock.elapsed());
    for row in frames {
        tokio::time::sleep(pace.wait_for_next(clock.elapsed())).await;
        draw(cmd, db, session, out, row, &clock, &mut run).await?;
        if !out.still_open() {
            break;
        }
        run.counters.late = pace.late();
        if let Some(line) = stats.tick(run.counters) {
            eprintln!("{line}");
        }
    }

    run.counters.late = pace.late();
    eprintln!("{}", stats.finish(run.counters));
    Ok(Played {
        diverged: run.diverged,
    })
}

/// What a run has done so far.
struct Run {
    counters: NativeCounters,
    diverged: Option<(u32, String, String)>,
}

/// One frame over probed state: fed, waited for, and put wherever this run
/// sends frames.
async fn draw(
    cmd: &DemoCmd,
    db: &Db,
    session: &mut Session,
    out: &mut Output,
    row: &schedule::FrameRow,
    clock: &Monotonic,
    run: &mut Run,
) -> Result<(), Failure> {
    session
        .feed_render(row.frame, row.tic, row.melt_step)
        .map_err(|err| failed(format!("feeding frame {}: {err}", row.frame)))?;
    let waited = match session.wait_frame(row.frame, FRAME_TIMEOUT).await {
        Ok(waited) => waited,
        // A statement the server gave up on takes rows and commits none, so
        // a frame that never lands is the only sign of it. Reopening the
        // statement and feeding the frame again is what tells a dead
        // statement from a slow one.
        //
        // Only that. A refused feed, a failed read or a frame that decoded
        // wrongly all say what went wrong already, and reopening the
        // statement would throw that away and report the retry's outcome
        // instead.
        Err(SessionError::FrameTimeout { .. }) => {
            let recovery = session
                .recover(&cmd.conn)
                .await
                .map_err(|err| failed(format!("frame {}: {err}", row.frame)))?;
            eprintln!(
                "# native reopened the renderer at frame {}, with {} tics committed",
                row.frame,
                recovery.resume_tic.saturating_sub(1)
            );
            session
                .feed_render(row.frame, row.tic, row.melt_step)
                .map_err(|err| failed(format!("feeding frame {}: {err}", row.frame)))?;
            session
                .wait_frame(row.frame, FRAME_TIMEOUT)
                .await
                .map_err(|err| failed(err.to_string()))?
        }
        Err(err) => return Err(failed(err.to_string())),
    };
    // A tic native_state marks unresolved or unimplemented drew whatever
    // was left of it rather than the tic itself, so the run stops before
    // going on to the tics after it. `wait_frame` already read this off
    // the same row as the frame, so the stop costs no query of its own.
    if let Some(refusal) = &waited.frame.refusal {
        return Err(gate(refusal.to_string()));
    }

    run.counters.tics += 1;
    run.counters.frames += 1;
    run.counters.render += waited.waited;
    run.counters.poll += waited.read;
    if run.diverged.is_none()
        && cmd.expect_probe_fbhash
        && waited.frame.fb_hash != row.probe_fb_hash
    {
        run.diverged = Some((
            row.frame,
            waited.frame.fb_hash.clone(),
            row.probe_fb_hash.clone(),
        ));
    }

    let before = clock.elapsed();
    out.draw(&waited.frame)?;
    let drawn = clock.elapsed();
    run.counters.blit += drawn.saturating_sub(before);
    out.write(db, &cmd.conn.database, row.frame, row.tic, &waited.frame)
        .await?;
    run.counters.write += clock.elapsed().saturating_sub(drawn);
    Ok(())
}

/// What stopped the simulation's own feeder loop short of the tic a frame
/// needed.
enum Stop {
    /// `native_state` marked the tic unresolved or unimplemented.
    Refused(Refusal),
    /// The tic ran past the demo lump's recorded commands.
    DemoEnded,
    /// Feeding or waiting for a tic failed outright.
    Failed(String),
}

/// One tic the feeder found, or how it stopped.
enum TicOutcome {
    Committed { elapsed: Duration },
    Stopped(Stop),
}

/// What the tic may take. The first pays for the statement's analysis,
/// which is seconds; every one after it is milliseconds.
fn sim_timeout(tic: u32) -> Duration {
    match tic {
        1 => FIRST_TIC_TIMEOUT,
        _ => TIC_TIMEOUT,
    }
}

/// Feeds the simulation the demo lump's commands, one tic at a time, as
/// fast as the statement takes them, and reports each one committed
/// through `tx`.
///
/// `tx`'s bound is the run's `--lookahead`: once it is full this blocks
/// until the paced loop has drawn another frame, which is what keeps the
/// simulation from running further ahead of the screen than that.
async fn feed_ahead(session: &Session, highest: Arc<AtomicU32>, tx: mpsc::Sender<TicOutcome>) {
    let mut tic = 1;
    loop {
        if let Err(err) = session.feed_sim(tic, tick::source::DEMO, 0, 0, 0) {
            let _ = tx
                .send(TicOutcome::Stopped(Stop::Failed(format!(
                    "feeding tic {tic}: {err}"
                ))))
                .await;
            return;
        }
        let ran = match session.wait_sim(tic, sim_timeout(tic)).await {
            Ok(ran) => ran,
            Err(err) => {
                let _ = tx
                    .send(TicOutcome::Stopped(Stop::Failed(err.to_string())))
                    .await;
                return;
            }
        };
        if let Some(refusal) = ran.refusal {
            let _ = tx.send(TicOutcome::Stopped(Stop::Refused(refusal))).await;
            return;
        }
        if ran.demo_end {
            let _ = tx.send(TicOutcome::Stopped(Stop::DemoEnded)).await;
            return;
        }
        highest.store(tic, Ordering::Relaxed);
        let sent = tx
            .send(TicOutcome::Committed {
                elapsed: ran.elapsed,
            })
            .await;
        // The paced loop stopped reading, at `--stop-at-frame` or the
        // window closing: nothing more to feed.
        if sent.is_err() {
            return;
        }
        tic += 1;
    }
}

/// Reads committed tics until `tic` is one of them, folding each into the
/// run's own tic count and simulation time as it goes.
async fn advance_to(
    rx: &mut mpsc::Receiver<TicOutcome>,
    highest: &AtomicU32,
    tic: u32,
    run: &mut Run,
) -> Result<(), Stop> {
    loop {
        drain_ready(rx, run)?;
        if highest.load(Ordering::Relaxed) >= tic {
            return Ok(());
        }
        match rx.recv().await {
            Some(TicOutcome::Committed { elapsed }) => {
                run.counters.tics += 1;
                if let Some(sim) = &mut run.counters.sim {
                    *sim += elapsed;
                }
            }
            Some(TicOutcome::Stopped(stop)) => return Err(stop),
            None => {
                return Err(Stop::Failed(
                    "the simulation feeder ended without a result".to_owned(),
                ));
            }
        }
    }
}

/// Folds every tic the feeder has already committed into the run's
/// counters, without waiting for one that has not arrived yet.
///
/// The feeder can run up to `--lookahead` tics ahead of what a frame
/// needs, and each of those sits in the channel uncounted until something
/// drains it: without this, `run.counters.tics` and the simulation time
/// only catch up whenever a later frame happens to need a tic far enough
/// ahead to drain past them, and the run's very last tics never get
/// credited at all.
fn drain_ready(rx: &mut mpsc::Receiver<TicOutcome>, run: &mut Run) -> Result<(), Stop> {
    loop {
        match rx.try_recv() {
            Ok(TicOutcome::Committed { elapsed }) => {
                run.counters.tics += 1;
                if let Some(sim) = &mut run.counters.sim {
                    *sim += elapsed;
                }
            }
            Ok(TicOutcome::Stopped(stop)) => return Err(stop),
            Err(mpsc::error::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                return Err(Stop::Failed(
                    "the simulation feeder ended without a result".to_owned(),
                ));
            }
        }
    }
}

/// The paced loop over the simulation's own output: a feeder runs the demo
/// lump's commands ahead of the screen, and this draws each frame once the
/// tic it reads is committed.
async fn play_sim(
    cmd: &DemoCmd,
    db: &Db,
    session: &Session,
    melt: &[MeltFrame],
    expect: Option<&HashMap<u32, String>>,
    out: &mut Output,
) -> Result<Played, Failure> {
    let melt_frames = melt.len() as u32;
    let melt_step = |frame: u32| melt.get(frame as usize).map_or(0, |m| m.melt_step);

    let clock = Monotonic::new();
    let mut run = Run {
        counters: NativeCounters {
            sim: Some(Duration::ZERO),
            lookahead: Some(0),
            ..NativeCounters::default()
        },
        diverged: None,
    };
    let (tx, rx) = mpsc::channel(cmd.lookahead.max(1) as usize);
    let highest = Arc::new(AtomicU32::new(0));
    let feeding = feed_ahead(session, Arc::clone(&highest), tx);

    // Owns `rx` and drops it the moment this is done drawing, whatever the
    // reason: `feed_ahead` blocks on a full channel, and only a dropped
    // receiver wakes it once nothing here is reading it any longer.
    let drawing = async move {
        let mut rx = rx;
        let result = drawing_loop(
            cmd,
            db,
            session,
            melt_frames,
            &melt_step,
            expect,
            out,
            &highest,
            &mut rx,
            &clock,
            &mut run,
        )
        .await;
        drop(rx);
        result
    };

    let (_, played) = tokio::join!(feeding, drawing);
    played
}

/// The paced draw loop's own body, over `rx`'s committed tics.
#[allow(clippy::too_many_arguments)]
async fn drawing_loop(
    cmd: &DemoCmd,
    db: &Db,
    session: &Session,
    melt_frames: u32,
    melt_step: &dyn Fn(u32) -> u8,
    expect: Option<&HashMap<u32, String>>,
    out: &mut Output,
    highest: &AtomicU32,
    rx: &mut mpsc::Receiver<TicOutcome>,
    clock: &Monotonic,
    run: &mut Run,
) -> Result<Played, Failure> {
    // Frame 0's tic pays for both statements' analysis, and is drawn
    // before the clock starts, the way every paced native run's first
    // frame is.
    let tic0 = schedule::sim_tic(0, melt_frames);
    match advance_to(rx, highest, tic0, run).await {
        Ok(()) => {}
        Err(Stop::DemoEnded) => return Ok(Played { diverged: None }),
        Err(Stop::Refused(refusal)) => return Err(gate(refusal.to_string())),
        Err(Stop::Failed(message)) => return Err(failed(message)),
    }
    draw_sim(
        cmd,
        db,
        session,
        out,
        0,
        tic0,
        melt_step(0),
        expect,
        clock,
        run,
    )
    .await?;
    eprintln!(
        "# native ready elapsed={:.1}s render={:.1}ms",
        clock.elapsed().as_secs_f64(),
        run.counters.render.as_secs_f64() * 1e3
    );

    let mut stats = NativeStatsLine::start(Monotonic::new(), STATS_INTERVAL, run.counters);
    let mut pace = Pace::start(TIC, clock.elapsed());
    let mut frame = 1u32;
    loop {
        if let Some(last) = cmd.stop_at_frame
            && frame > last
        {
            break;
        }
        let tic = schedule::sim_tic(frame, melt_frames);
        match advance_to(rx, highest, tic, run).await {
            Ok(()) => {}
            Err(Stop::DemoEnded) => break,
            Err(Stop::Refused(refusal)) => return Err(gate(refusal.to_string())),
            Err(Stop::Failed(message)) => return Err(failed(message)),
        }
        tokio::time::sleep(pace.wait_for_next(clock.elapsed())).await;
        draw_sim(
            cmd,
            db,
            session,
            out,
            frame,
            tic,
            melt_step(frame),
            expect,
            clock,
            run,
        )
        .await?;
        if !out.still_open() {
            break;
        }
        run.counters.lookahead = Some(highest.load(Ordering::Relaxed).saturating_sub(tic));
        run.counters.late = pace.late();
        if let Some(line) = stats.tick(run.counters) {
            eprintln!("{line}");
        }
        frame += 1;
    }

    // Whatever the feeder had already committed beyond what the last frame
    // needed, so the closing line counts every tic it actually ran ahead
    // to. A stop past the last frame this run asked for is not this run's
    // to report.
    let _ = drain_ready(rx, run);
    run.counters.late = pace.late();
    eprintln!("{}", stats.finish(run.counters));
    Ok(Played {
        diverged: run.diverged.take(),
    })
}

/// One frame over the simulation's own output: fed, waited for, and put
/// wherever this run sends frames.
///
/// The simulation statement dying is not recovered here: `feed_ahead`
/// reports it as a failure and this run ends, the same as `native diff`
/// does over a dead statement.
#[allow(clippy::too_many_arguments)]
async fn draw_sim(
    cmd: &DemoCmd,
    db: &Db,
    session: &Session,
    out: &mut Output,
    frame: u32,
    tic: u32,
    melt_step: u8,
    expect: Option<&HashMap<u32, String>>,
    clock: &Monotonic,
    run: &mut Run,
) -> Result<(), Failure> {
    session
        .feed_render(frame, tic, melt_step)
        .map_err(|err| failed(format!("feeding frame {frame}: {err}")))?;
    let waited = session
        .wait_frame(frame, FRAME_TIMEOUT)
        .await
        .map_err(|err| failed(err.to_string()))?;
    if let Some(refusal) = &waited.frame.refusal {
        return Err(gate(refusal.to_string()));
    }

    run.counters.frames += 1;
    run.counters.render += waited.waited;
    run.counters.poll += waited.read;
    if run.diverged.is_none()
        && let Some(theirs) = expect.and_then(|expect| expect.get(&frame))
        && waited.frame.fb_hash != *theirs
    {
        run.diverged = Some((frame, waited.frame.fb_hash.clone(), theirs.clone()));
    }

    let before = clock.elapsed();
    out.draw(&waited.frame)?;
    let drawn = clock.elapsed();
    run.counters.blit += drawn.saturating_sub(before);
    out.write(db, &cmd.conn.database, frame, tic, &waited.frame)
        .await?;
    run.counters.write += clock.elapsed().saturating_sub(drawn);
    Ok(())
}

/// Where each frame goes: the screen, a PPM, a hash file, or nothing.
struct Output {
    window: Option<Window>,
    frame_dir: Option<PathBuf>,
    hashes: Option<(PathBuf, std::io::BufWriter<std::fs::File>)>,
}

impl Output {
    fn open(cmd: &DemoCmd) -> Result<Output, Failure> {
        let window = match cmd.no_window {
            true => None,
            false => Some(
                Window::open(&format!("ClickDOOM {}", cmd.demo), cmd.scale)
                    .map_err(|err| failed(err.to_string()))?,
            ),
        };
        if let Some(dir) = &cmd.frame_dir {
            std::fs::create_dir_all(dir)
                .map_err(|err| failed(format!("creating {}: {err}", dir.display())))?;
        }
        let hashes = match &cmd.hash_out {
            None => None,
            Some(path) => {
                let file = std::fs::File::create(path)
                    .map_err(|err| failed(format!("creating {}: {err}", path.display())))?;
                let mut writer = std::io::BufWriter::new(file);
                writeln!(writer, "frame\ttic\tfb_hash")
                    .map_err(|err| failed(format!("writing {}: {err}", path.display())))?;
                Some((path.clone(), writer))
            }
        };
        Ok(Output {
            window,
            frame_dir: cmd.frame_dir.clone(),
            hashes,
        })
    }

    /// Puts one frame on the screen, as the bytes SQL produced.
    fn draw(&mut self, frame: &crate::native::Frame) -> Result<(), Failure> {
        let Some(window) = &mut self.window else {
            return Ok(());
        };
        window
            .draw(&frame.rgb32)
            .map_err(|err| failed(err.to_string()))
    }

    /// Writes one frame to the files this run keeps. The PPM is a query per
    /// frame, so a run that writes them is not a run at 35 Hz.
    async fn write(
        &mut self,
        db: &Db,
        database: &str,
        frame_index: u32,
        tic: u32,
        frame: &crate::native::Frame,
    ) -> Result<(), Failure> {
        if let Some(dir) = &self.frame_dir {
            write_ppm(db, database, frame_index, dir).await?;
        }
        if let Some((path, writer)) = &mut self.hashes {
            writeln!(writer, "{frame_index}\t{tic}\t{}", frame.fb_hash)
                .map_err(|err| failed(format!("writing {}: {err}", path.display())))?;
        }
        Ok(())
    }

    /// Whether the run should go on. A run with no window runs to its end.
    fn still_open(&self) -> bool {
        match &self.window {
            Some(window) => window.is_open(),
            None => true,
        }
    }

    fn finish(self) -> Result<(), Failure> {
        let Some((path, mut writer)) = self.hashes else {
            return Ok(());
        };
        writer
            .flush()
            .map_err(|err| failed(format!("writing {}: {err}", path.display())))?;
        println!("{}", path.display());
        Ok(())
    }
}

/// One frame as a binary PPM, built whole in SQL. Naming the file after the
/// frame rather than a counter means a re-run rewrites the frame it drew
/// again instead of renumbering the ones after it.
async fn write_ppm(db: &Db, database: &str, frame: u32, dir: &Path) -> Result<(), Failure> {
    let source = |column| {
        format!("SELECT {column} FROM {database}.native_frames WHERE frame = {frame} LIMIT 1")
    };
    let ppm: bytes::Bytes = db
        .fetch_one(&ppm_sql_over(
            &source("fb"),
            &source("palette"),
            FB_WIDTH,
            FB_HEIGHT,
        ))
        .await
        .map_err(|err| failed(format!("reading frame {frame} as a PPM: {err}")))?;
    let path = dir.join(format!("frame-{frame:05}.ppm"));
    std::fs::write(&path, &ppm).map_err(|err| failed(format!("writing {}: {err}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Only {
        #[command(flatten)]
        demo: DemoCmd,
    }

    fn parsed(args: &[&str]) -> DemoCmd {
        let mut all = vec!["demo"];
        all.extend_from_slice(args);
        Only::try_parse_from(all).expect("the arguments parse").demo
    }

    #[test]
    fn the_common_case_is_one_word() {
        let cmd = parsed(&[]);
        assert_eq!(cmd.demo, "demo3");
        assert_eq!(cmd.scale, Scale::Two);
        assert!(!cmd.no_window);
        assert!(cmd.stop_at_frame.is_none());
        assert_eq!(cmd.from, Source::Probe);
        assert_eq!(cmd.lookahead, 35);
    }

    #[test]
    fn the_demo_is_positional_and_the_rest_are_flags() {
        let cmd = parsed(&[
            "demo1",
            "--no-window",
            "--scale",
            "4",
            "--stop-at-frame",
            "40",
            "--from",
            "sim",
            "--lookahead",
            "10",
        ]);
        assert_eq!(cmd.demo, "demo1");
        assert!(cmd.no_window);
        assert_eq!(cmd.scale, Scale::Four);
        assert_eq!(cmd.stop_at_frame, Some(40));
        assert_eq!(cmd.from, Source::Sim);
        assert_eq!(cmd.lookahead, 10);
    }

    /// The window scales the engine's own resolution, so the values are the
    /// ones the window system can do exactly.
    #[test]
    fn the_scale_is_one_of_the_three_the_window_draws() {
        for scale in ["1", "2", "4"] {
            assert!(
                Only::try_parse_from(["demo", "--scale", scale]).is_ok(),
                "{scale}"
            );
        }
        assert!(Only::try_parse_from(["demo", "--scale", "3"]).is_err());
    }
}
