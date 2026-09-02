//! `clickdoom native render`: one frame, to a file or to a hash.

use std::path::PathBuf;
use std::time::Duration; // purity-ok: a frame timeout and the timings the session measured, read from no clock here

use crate::cli::{Exit, Failure, failed, gate};
use crate::client::{ConnArgs, Db};
use crate::native::{Session, schedule};
use crate::render::{FB_HEIGHT, FB_WIDTH, ppm_sql_over};
use clap::{Args, ValueEnum};

/// How long one frame may take before the run calls the renderer dead. A
/// frame averages 15.7 ms, so this is a wide margin over the slowest and
/// not a target.
const FRAME_TIMEOUT: Duration = Duration::from_secs(60);

/// Where a run's state rows come from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Source {
    /// Rows the reference emulator's probe recorded, loaded by
    /// `clickdoom native load --probe`
    Probe,
}

#[derive(Args)]
#[command(
    about = "Render one frame and write it out or check its hash",
    // Hard-wrapped: clap only rewraps help text with its `wrap_help`
    // feature, which this binary does not enable.
    long_about = "\
Open the renderer as a resident statement, feed it every frame up to --frame
in order, and read the last one back. A frame draws over the one before it,
so the run has to start at frame 0; the screen melt's frames draw over black
instead, and how far the wipe has got comes from the melt schedule the load
put in the database.

Which tic each frame is drawn from comes from the probe rows, not from
arithmetic here.

With no output flag this prints the frame's hash and what the run cost.
--fb-hash prints the 16 hex digits alone, for a script. --ppm writes the
frame as a binary PPM, built in SQL from the framebuffer and the palette.

--expect-fbhash names the hash the last frame has to have.
--expect-probe-fbhash checks every frame the run draws against the hash the
probe recorded for it, so a re-generated probe leaves no stale constant
behind in a caller and a frame that differs is caught where it differs.

Exit codes: 0 the frame rendered, 1 the run failed, 3 the frame did not hash
to what was expected."
)]
pub struct RenderCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// Where the state rows come from
    #[arg(long, value_enum, default_value_t = Source::Probe)]
    pub from: Source,
    /// The frame to render
    #[arg(long)]
    pub frame: u32,
    /// Write the frame here as a binary PPM
    #[arg(long, value_name = "PATH")]
    pub ppm: Option<PathBuf>,
    /// Print the frame's hash alone
    #[arg(long)]
    pub fb_hash: bool,
    /// Fail with exit 3 unless the frame hashes to this
    #[arg(long, value_name = "HEX")]
    pub expect_fbhash: Option<String>,
    /// Fail with exit 3 unless the frame hashes to what the probe recorded
    /// for it
    #[arg(long, conflicts_with = "expect_fbhash")]
    pub expect_probe_fbhash: bool,
}

pub(crate) async fn run(cmd: &RenderCmd) -> Result<Exit, Failure> {
    let database = &cmd.conn.database;
    let db = cmd.conn.connect();
    let Source::Probe = cmd.from;
    let plan = schedule::from_probe(&db, database, Some(cmd.frame))
        .await
        .map_err(|err| failed(err.to_string()))?;

    schedule::clear_frames(&db, database)
        .await
        .map_err(|err| failed(format!("emptying the frames table: {err}")))?;
    let session = Session::open(
        &cmd.conn,
        database,
        None,
        &clickdoom_native::sql::render::frame_transform(database),
    )
    .await
    .map_err(|err| failed(format!("opening the renderer: {err}")))?;

    let rendered = render_up_to(&session, &plan, cmd.expect_probe_fbhash).await;
    // The statement goes whether the run worked or not, and its own error
    // is the better one when both fail.
    let closed = session.close().await;
    let rendered = match (rendered, closed) {
        (Ok(rendered), Ok(())) => rendered,
        (_, Err(err)) => return Err(failed(format!("the renderer statement failed: {err}"))),
        (Err(failure), Ok(())) => return Err(failure),
    };

    if let Some(path) = &cmd.ppm {
        write_ppm(&db, database, cmd.frame, path).await?;
    }
    report(cmd, &rendered);
    check(cmd, &rendered)
}

/// What the run produced.
struct Rendered {
    fb_hash: String,
    /// Frames fed, including the one asked for.
    frames: usize,
    /// How long the whole run took, and how long the frame asked for took.
    elapsed: Duration,
    last: Duration,
    /// The first frame that did not hash to what the probe recorded, and
    /// what it hashed to instead.
    diverged: Option<(u32, String, String)>,
}

/// Feeds every frame in order and returns what the last one came back as.
///
/// `against_probe` compares every frame as it lands, not only the last: a
/// frame the melt draws can differ while the frame after it, which redraws
/// the whole screen, comes out the same.
async fn render_up_to(
    session: &Session,
    plan: &[schedule::FrameRow],
    against_probe: bool,
) -> Result<Rendered, Failure> {
    let mut elapsed = Duration::ZERO;
    let mut last = Duration::ZERO;
    let mut fb_hash = String::new();
    let mut diverged = None;
    for row in plan {
        session
            .feed_render(row.frame, row.tic, row.melt_step)
            .map_err(|err| failed(format!("feeding frame {}: {err}", row.frame)))?;
        let (frame, took) = session
            .wait_frame(row.frame, FRAME_TIMEOUT)
            .await
            .map_err(|err| failed(err.to_string()))?;
        elapsed += took;
        last = took;
        fb_hash = frame.fb_hash;
        if against_probe && diverged.is_none() && fb_hash != row.probe_fb_hash {
            diverged = Some((row.frame, fb_hash.clone(), row.probe_fb_hash.clone()));
        }
    }
    Ok(Rendered {
        fb_hash,
        frames: plan.len(),
        elapsed,
        last,
        diverged,
    })
}

/// Writes the frame as a binary PPM. SQL builds the whole file, header
/// included, out of the framebuffer and the palette it stored; nothing here
/// looks at a pixel.
async fn write_ppm(
    db: &Db,
    database: &str,
    frame: u32,
    path: &std::path::Path,
) -> Result<(), Failure> {
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
    std::fs::write(path, &ppm).map_err(|err| failed(format!("writing {}: {err}", path.display())))
}

fn report(cmd: &RenderCmd, rendered: &Rendered) {
    if cmd.fb_hash {
        println!("{}", rendered.fb_hash);
        return;
    }
    println!(
        "frame {} fb_hash={} frames={} render={:.1}ms total={:.2}s",
        cmd.frame,
        rendered.fb_hash,
        rendered.frames,
        rendered.last.as_secs_f64() * 1e3,
        rendered.elapsed.as_secs_f64()
    );
    if let Some(path) = &cmd.ppm {
        println!("{}", path.display());
    }
}

/// The gate: every frame hashed to what it was expected to.
fn check(cmd: &RenderCmd, rendered: &Rendered) -> Result<Exit, Failure> {
    if let Some((frame, ours, theirs)) = &rendered.diverged {
        return Err(gate(format!(
            "frame {frame} hashes to {ours}, not the {theirs} the probe \
             recorded. The frame the renderer drew is not the one the engine \
             drew"
        )));
    }
    let Some(want) = &cmd.expect_fbhash else {
        return Ok(Exit::Ok);
    };
    let want = want.to_ascii_lowercase();
    if rendered.fb_hash == want {
        return Ok(Exit::Ok);
    }
    Err(gate(format!(
        "frame {} hashes to {}, not {want}. The frame the renderer drew is \
         not the one the engine drew",
        cmd.frame, rendered.fb_hash
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Only {
        #[command(flatten)]
        render: RenderCmd,
    }

    fn parsed(args: &[&str]) -> RenderCmd {
        let mut all = vec!["render"];
        all.extend_from_slice(args);
        Only::try_parse_from(all)
            .expect("the arguments parse")
            .render
    }

    fn rendered(fb_hash: &str) -> Rendered {
        Rendered {
            fb_hash: fb_hash.to_owned(),
            frames: 1,
            elapsed: Duration::ZERO,
            last: Duration::ZERO,
            diverged: None,
        }
    }

    #[test]
    fn a_frame_is_required_and_the_source_defaults_to_the_probe() {
        assert!(Only::try_parse_from(["render"]).is_err());
        let cmd = parsed(&["--frame", "40"]);
        assert_eq!(cmd.frame, 40);
        assert_eq!(cmd.from, Source::Probe);
        assert!(cmd.expect_fbhash.is_none());
    }

    #[test]
    fn a_hash_given_on_the_command_line_is_matched_whatever_case_it_is_in() {
        let cmd = parsed(&["--frame", "40", "--expect-fbhash", "2EB87849EE6D9714"]);
        assert_eq!(
            check(&cmd, &rendered("2eb87849ee6d9714")).map_err(|f| f.exit),
            Ok(Exit::Ok)
        );
    }

    #[test]
    fn a_mismatch_is_a_gate_failure_naming_both_hashes() {
        let cmd = parsed(&["--frame", "40", "--expect-fbhash", "2eb87849ee6d9714"]);
        let failure = check(&cmd, &rendered("0000000000000000")).expect_err("a mismatch");
        assert_eq!(failure.exit, Exit::Gate);
        assert!(
            failure.message.contains("2eb87849ee6d9714"),
            "{}",
            failure.message
        );
        assert!(
            failure.message.contains("0000000000000000"),
            "{}",
            failure.message
        );
    }

    /// A frame partway through the run is what the probe check is for: the
    /// last frame can agree while an earlier one does not.
    #[test]
    fn a_frame_that_diverged_earlier_is_reported_over_the_last_one() {
        let cmd = parsed(&["--frame", "40", "--expect-probe-fbhash"]);
        let mut rendered = rendered("2eb87849ee6d9714");
        rendered.diverged = Some((
            0,
            "0000000000000000".to_owned(),
            "fe5d82c0f42d45f1".to_owned(),
        ));
        let failure = check(&cmd, &rendered).expect_err("an earlier frame diverged");
        assert_eq!(failure.exit, Exit::Gate);
        assert!(failure.message.contains("frame 0"), "{}", failure.message);
        assert!(
            failure.message.contains("fe5d82c0f42d45f1"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn nothing_is_checked_when_nothing_is_expected() {
        let cmd = parsed(&["--frame", "40"]);
        assert_eq!(
            check(&cmd, &rendered("whatever")).map_err(|f| f.exit),
            Ok(Exit::Ok)
        );
    }

    /// One source for the expected hash, so a caller cannot name one and
    /// ask for the other's.
    #[test]
    fn the_two_ways_of_expecting_a_hash_are_exclusive() {
        assert!(
            Only::try_parse_from([
                "render",
                "--frame",
                "40",
                "--expect-fbhash",
                "0",
                "--expect-probe-fbhash"
            ])
            .is_err()
        );
    }
}
