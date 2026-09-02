//! `clickdoom native play`: the level, driven by the keyboard and mouse.

use std::time::Duration; // purity-ok: the frame budget and the timings the session measured, read from no clock here

use clap::Args;
use clickdoom_native::sql::sim::tick;

use crate::cli::{Exit, Failure, failed};
use crate::client::ConnArgs;
use crate::native::pace::{Pace, TIC};
use crate::native::session::TIC_TIMEOUT;
use crate::native::window::{Scale, Window};
use crate::native::{Session, schedule};
use crate::stats::{Clock, Monotonic, NativeCounters, NativeStatsLine};

/// How long one tic or one frame may take before the run calls the
/// statement dead. The first of each pays for the statement's analysis and
/// its scalar constants, which is seconds.
const FRAME_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the progress line comes out.
const STATS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Args)]
#[command(
    about = "Play the loaded level from the keyboard and mouse",
    // Hard-wrapped: clap only rewraps help text with its `wrap_help`
    // feature, which this binary does not enable.
    long_about = "\
Sample the keys and the mouse once per tic, stream them as the tic's input
row, and draw the state row the simulation writes from them. One tic per
frame on the fixed 35 Hz clock, and no lookahead: a tic cannot be run before
the key that goes into it has been pressed.

The key bits are clickdoom_spec::native_state::key and the SQL side builds
the tic command out of them, the way G_BuildTiccmd does. Arrows or WASD
move, Ctrl fires, Space uses, Shift runs, Alt strafes, 1 to 7 select a
weapon, P pauses and Escape ends the run.

The pointer is not captured, so mouse movement stops at the edge of the
window.

Exit codes: 0 the run finished, 1 it failed."
)]
pub struct PlayCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// How much bigger than 320x200 the window is drawn
    #[arg(long, value_enum, default_value = "2")]
    pub scale: Scale,
    /// Stop after this many tics
    #[arg(long, value_name = "N")]
    pub max_tics: Option<u32>,
}

pub(crate) async fn run(cmd: &PlayCmd) -> Result<Exit, Failure> {
    let database = &cmd.conn.database;
    let db = cmd.conn.connect();
    let mut window = Window::open("ClickDOOM", cmd.scale).map_err(|err| failed(err.to_string()))?;

    schedule::clear_frames(&db, database)
        .await
        .map_err(|err| failed(format!("emptying the frames table: {err}")))?;
    let session = Session::open(
        &cmd.conn,
        database,
        Some(&tick::resident_statement(database)),
        &clickdoom_native::sql::render::frame_transform(database),
    )
    .await
    .map_err(|err| failed(format!("opening the session: {err}")))?;

    let first = session
        .resume_point()
        .await
        .map_err(|err| failed(format!("reading the tic to resume from: {err}")))?;
    println!(
        "{}:{}/{database}: from tic {first}, at {:.1} Hz",
        cmd.conn.host,
        cmd.conn.port,
        1.0 / TIC.as_secs_f64()
    );

    let played = play(cmd, &session, &mut window, first).await;
    let closed = session.close().await;
    match (played, closed) {
        (Ok(()), Ok(())) => Ok(Exit::Ok),
        (_, Err(err)) => Err(failed(format!("a statement failed: {err}"))),
        (Err(failure), Ok(())) => Err(failure),
    }
}

/// The paced loop: sample, then run the next tic and draw this one's frame
/// at the same time.
///
/// The two are independent within a tic. The frame for tic t reads the state
/// row for t, and the tic after t advances from that same row, so neither
/// needs the other's result and the tic costs whichever of them is slower
/// rather than both.
///
/// No lookahead past that. The demo loop can run the simulation ahead of
/// what is on the screen because the input is already recorded; here the
/// command for tic t + 1 is whatever the keyboard says while frame t is
/// being drawn, which is as early as it can be known.
async fn play(
    cmd: &PlayCmd,
    session: &Session,
    window: &mut Window,
    first: u32,
) -> Result<(), Failure> {
    let clock = Monotonic::new();
    let mut counters = NativeCounters {
        sim: Some(Duration::ZERO),
        lookahead: Some(0),
        ..NativeCounters::default()
    };
    warm(session, window, first, &mut counters).await?;
    eprintln!(
        "# native ready elapsed={:.1}s sim={:.1}ms render={:.1}ms",
        clock.elapsed().as_secs_f64(),
        counters.sim.unwrap_or_default().as_secs_f64() * 1e3,
        counters.render.as_secs_f64() * 1e3
    );

    let mut stats = NativeStatsLine::start(Monotonic::new(), STATS_INTERVAL, counters);
    let mut pace = Pace::start(TIC, clock.elapsed());
    let last = cmd.max_tics.map(|tics| first.saturating_add(tics) - 1);
    let mut tic = first;
    while window.is_open() && last.is_none_or(|last| tic <= last) {
        // Sampled once per tic, so a key held across a tic boundary lands
        // in one tic rather than in the two either side of it.
        let (keys, mouse_dx, mouse_dy) = sample(window);
        let next = tic + 1;

        // Both statements read the state row for `tic`, which is already
        // committed: the frame draws it and the next tic advances from it.
        // Neither waits on the other, so they are fed together and waited
        // for together.
        session
            .feed_sim(next, tick::source::KEYS, keys, mouse_dx, mouse_dy)
            .map_err(|err| failed(format!("feeding tic {next}: {err}")))?;
        session
            .feed_render(tic, tic, 0)
            .map_err(|err| failed(format!("feeding frame {tic}: {err}")))?;
        let (ran, waited) = tokio::join!(
            session.wait_sim(next, TIC_TIMEOUT),
            session.wait_frame(tic, FRAME_TIMEOUT)
        );
        let ran = ran.map_err(|err| failed(err.to_string()))?;
        let waited = waited.map_err(|err| failed(err.to_string()))?;

        let before = clock.elapsed();
        window
            .draw(&waited.frame.rgb32)
            .map_err(|err| failed(err.to_string()))?;
        counters.blit += clock.elapsed().saturating_sub(before);
        counters.frames += 1;
        counters.render += waited.waited;
        counters.poll += waited.read;
        counters.sim = counters.sim.map(|total| total + ran);
        counters.tics += 1;

        counters.late = pace.late();
        if let Some(line) = stats.tick(counters) {
            eprintln!("{line}");
        }
        tokio::time::sleep(pace.wait_for_next(clock.elapsed())).await;
        tic = next;
    }

    counters.late = pace.late();
    eprintln!("{}", stats.finish(counters));
    Ok(())
}

/// Runs the first tic and draws the first frame, before the clock starts.
///
/// Both statements are analysed on their first row and each takes seconds.
/// They are fed before either is waited for, so the two are analysed at
/// once rather than one after the other, which is the difference between
/// one wait and two before the first frame.
///
/// The frame is the level as tic 0 leaves it, which is the row the load
/// wrote, so the renderer needs nothing from the simulation to draw it.
async fn warm(
    session: &Session,
    window: &mut Window,
    first: u32,
    counters: &mut NativeCounters,
) -> Result<(), Failure> {
    let (keys, mouse_dx, mouse_dy) = sample(window);
    session
        .feed_render(0, 0, 0)
        .map_err(|err| failed(format!("feeding frame 0: {err}")))?;
    session
        .feed_sim(first, tick::source::KEYS, keys, mouse_dx, mouse_dy)
        .map_err(|err| failed(format!("feeding tic {first}: {err}")))?;

    let (drawn, ran) = tokio::join!(
        session.wait_frame(0, FRAME_TIMEOUT),
        session.wait_sim(first, FRAME_TIMEOUT)
    );
    let drawn = drawn.map_err(|err| failed(err.to_string()))?;
    let ran = ran.map_err(|err| failed(err.to_string()))?;
    window
        .draw(&drawn.frame.rgb32)
        .map_err(|err| failed(err.to_string()))?;

    counters.tics += 1;
    counters.frames += 1;
    counters.render += drawn.waited;
    counters.poll += drawn.read;
    counters.sim = Some(ran);
    Ok(())
}

/// The keys and the mouse as they stand, ferried through unchanged.
fn sample(window: &mut Window) -> (u32, i16, i16) {
    let keys = window.keys();
    let (dx, dy) = window.mouse();
    (keys, dx, dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Only {
        #[command(flatten)]
        play: PlayCmd,
    }

    fn parsed(args: &[&str]) -> PlayCmd {
        let mut all = vec!["play"];
        all.extend_from_slice(args);
        Only::try_parse_from(all).expect("the arguments parse").play
    }

    #[test]
    fn a_run_needs_nothing_but_the_connection() {
        let cmd = parsed(&[]);
        assert_eq!(cmd.scale, Scale::Two);
        assert!(cmd.max_tics.is_none());
    }

    /// `--scale` means the same here as it does for a demo, so a caller
    /// moving between the two does not have to look it up.
    #[test]
    fn the_scale_is_the_one_the_demo_takes() {
        assert_eq!(parsed(&["--scale", "4"]).scale, Scale::Four);
        assert!(Only::try_parse_from(["play", "--scale", "3"]).is_err());
    }

    #[test]
    fn max_tics_counts_from_where_the_run_resumes() {
        let cmd = parsed(&["--max-tics", "10"]);
        assert_eq!(cmd.max_tics, Some(10));
    }
}
