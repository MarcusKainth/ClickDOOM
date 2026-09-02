//! `clickdoom native demo`: a demo played at 35 Hz, in a window.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration; // purity-ok: the frame budget and the timings the session measured, read from no clock here

use clap::{Args, ValueEnum};

use crate::cli::{Exit, Failure, failed, gate};
use crate::client::{ConnArgs, Db};
use crate::native::pace::{Pace, TIC};
use crate::native::window::{Scale, Window};
use crate::native::{Session, SessionError, schedule};
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

Which tic each frame is drawn from, and how far the screen melt has got,
come from the rows the load put in the database.

--no-window runs headless, which is what --frame-dir and --hash-out are for:
a PPM per frame, built in SQL, and a TSV of frame, tic and frame hash. The
PPM is a query of its own per frame and the progress line reports what it
costs, so a run that writes them does not hold 35 Hz. --stop-at-frame ends
the run early; without it the run ends with the demo.

Exit codes: 0 the run finished, 1 it failed, 3 --expect-probe-fbhash found a
frame the engine did not draw."
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
}

pub(crate) async fn run(cmd: &DemoCmd) -> Result<Exit, Failure> {
    let database = &cmd.conn.database;
    let db = cmd.conn.connect();
    let Source::Probe = cmd.from;
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
    let played = play(cmd, &db, &mut session, &plan, &mut out).await;
    let closed = session.close().await;
    let played = match (played, closed) {
        (Ok(played), Ok(())) => played,
        (_, Err(err)) => return Err(failed(format!("the renderer statement failed: {err}"))),
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

/// What the run ended with.
struct Played {
    diverged: Option<(u32, String, String)>,
}

/// The paced loop: one frame per tic, on the clock and not on how long the
/// frame took.
///
/// The first frame is drawn before the clock starts. It pays for the
/// statement's analysis and for evaluating its scalar constants, which is
/// seconds, and pacing against it would leave every frame after it late
/// against a deadline that is already minutes in the past.
async fn play(
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

/// What the run has done so far.
struct Run {
    counters: NativeCounters,
    diverged: Option<(u32, String, String)>,
}

/// One frame: fed, waited for, and put wherever this run sends frames.
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
    out.write(db, &cmd.conn.database, row, &waited.frame)
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
        row: &schedule::FrameRow,
        frame: &crate::native::Frame,
    ) -> Result<(), Failure> {
        if let Some(dir) = &self.frame_dir {
            write_ppm(db, database, row.frame, dir).await?;
        }
        if let Some((path, writer)) = &mut self.hashes {
            writeln!(writer, "{}\t{}\t{}", row.frame, row.tic, frame.fb_hash)
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
        ]);
        assert_eq!(cmd.demo, "demo1");
        assert!(cmd.no_window);
        assert_eq!(cmd.scale, Scale::Four);
        assert_eq!(cmd.stop_at_frame, Some(40));
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
