//! Which frames a run renders, what each one reads, and the table they land
//! in.
//!
//! The renderer's input row is `(frame, tic, melt_step)`. For a run over
//! probed states all three are already in the database: `probe_state` says
//! which tic each frame was drawn from, and `melt_schedule` how far the
//! wipe had got. One query pairs them, so the driver streams rows it read
//! rather than rows it worked out.

use crate::client::{self, Db};
use crate::native::melt;
use crate::native::probe;
use crate::native::session::FRAMES_TABLE;

/// Anything that stops a run from knowing which frames to render.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] client::Error),
    #[error(
        "{database}.{} holds no frames. Load the reference emulator's rows \
         first: clickdoom native load --probe PATH",
        probe::STAGING_TABLE
    )]
    NoProbe { database: String },
    #[error(
        "{database}.{} holds no frame {frame}. It runs to frame {last}",
        probe::STAGING_TABLE
    )]
    NoSuchFrame {
        database: String,
        frame: u32,
        last: u32,
    },
    #[error(
        "frame {frame} draws over frame {}, which {database}.{} does not \
         hold. Load a probe that covers every frame up to {frame}",
        frame - 1,
        probe::STAGING_TABLE
    )]
    NoPreviousFrame { database: String, frame: u32 },
}

/// One row of the renderer's input, with the hash the probe recorded for
/// the frame it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRow {
    pub frame: u32,
    /// The tic the frame is drawn from.
    pub tic: u32,
    /// How far the screen melt has advanced, 0 once it is over.
    pub melt_step: u8,
    /// The 16 hex digits the engine's own frame hashed to.
    pub probe_fb_hash: String,
}

/// Every frame the probe recorded, in order, each with the tic it was drawn
/// from and its melt step.
///
/// `last` cuts the run short at that frame. A frame the probe does not hold
/// is an error rather than a short list, because a run that stops early
/// looks the same as one that finished.
pub async fn from_probe(
    db: &Db,
    database: &str,
    last: Option<u32>,
) -> Result<Vec<FrameRow>, Error> {
    let highest = highest_frame(db, database).await?;
    if let Some(last) = last
        && last > highest
    {
        return Err(Error::NoSuchFrame {
            database: database.to_owned(),
            frame: last,
            last: highest,
        });
    }
    let cut = last.unwrap_or(highest);
    let rows: Vec<(u32, u32, u8, String)> = db
        .fetch_all(&format!(
            "SELECT p.frame_index, p.gametic, toUInt8(ifNull(m.melt_step, 0)), p.fb_hash \
             FROM (SELECT frame_index, gametic, lower(fb_hash) AS fb_hash \
                   FROM {database}.{} WHERE frame_index <= {cut}) AS p \
             LEFT JOIN {database}.{} AS m ON p.frame_index = m.frame \
             ORDER BY p.frame_index \
             SETTINGS join_use_nulls = 1",
            probe::STAGING_TABLE,
            melt::TABLE
        ))
        .await?;
    let plan: Vec<FrameRow> = rows
        .into_iter()
        .map(|(frame, tic, melt_step, probe_fb_hash)| FrameRow {
            frame,
            tic,
            melt_step,
            probe_fb_hash,
        })
        .collect();
    contiguous(database, &plan)?;
    Ok(plan)
}

/// Every frame the run draws over is one the run draws.
///
/// A frame is the frame before it with this frame's pixels cut in, so a gap
/// in the plan means drawing over whatever the frames table happened to
/// hold. A melt frame draws over the black screen the wipe started from and
/// needs nothing before it.
fn contiguous(database: &str, plan: &[FrameRow]) -> Result<(), Error> {
    for (at, row) in plan.iter().enumerate() {
        if row.melt_step > 0 || row.frame == 0 {
            continue;
        }
        let previous = at.checked_sub(1).map(|before| plan[before].frame);
        if previous != Some(row.frame - 1) {
            return Err(Error::NoPreviousFrame {
                database: database.to_owned(),
                frame: row.frame,
            });
        }
    }
    Ok(())
}

/// Empties the frames table.
///
/// `native_frames` keys on the frame, so a second run over the same frames
/// would read the first run's rows back before its own land. A run renders
/// from frame 0, so it starts from an empty table.
pub async fn clear_frames(db: &Db, database: &str) -> Result<(), client::Error> {
    db.run(&format!(
        "TRUNCATE TABLE IF EXISTS {database}.{FRAMES_TABLE}"
    ))
    .await
}

/// The last frame the probe holds.
async fn highest_frame(db: &Db, database: &str) -> Result<u32, Error> {
    let (rows, highest) = db
        .fetch_one::<(u64, u32)>(&format!(
            "SELECT count(), max(frame_index) FROM {database}.{}",
            probe::STAGING_TABLE
        ))
        .await
        .map_err(|source| match table_is_missing(&source) {
            true => Error::NoProbe {
                database: database.to_owned(),
            },
            false => Error::Db(source),
        })?;
    match rows {
        0 => Err(Error::NoProbe {
            database: database.to_owned(),
        }),
        _ => Ok(highest),
    }
}

/// Whether the server refused a query because the table is not there.
/// ClickHouse says so in the message; the code it carries is not on the
/// error this client hands back.
fn table_is_missing(error: &client::Error) -> bool {
    error.to_string().contains("UNKNOWN_TABLE")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transform declares its own `input(...)`, and a session encodes
    /// the rows for it from a schema of its own. A drift between the two
    /// puts every field in the wrong column, which shows up as a rendered
    /// frame rather than as an error.
    #[test]
    fn the_renderer_reads_the_rows_a_session_sends_it() {
        use crate::native::session::RENDER_INPUT_SCHEMA;
        let sql = clickdoom_native::sql::render::frame_transform("nat");
        assert!(
            sql.contains(&format!("input('{RENDER_INPUT_SCHEMA}')")),
            "the transform does not read {RENDER_INPUT_SCHEMA}"
        );
        assert!(
            sql.contains("WHERE empty(pad)"),
            "the padding row is dropped by something other than its pad column"
        );
    }

    #[test]
    fn a_missing_probe_says_which_command_fills_it() {
        let error = Error::NoProbe {
            database: "nat".to_owned(),
        };
        let message = error.to_string();
        assert!(message.contains("nat.probe_state"), "{message}");
        assert!(message.contains("native load --probe"), "{message}");
    }

    fn row(frame: u32, melt_step: u8) -> FrameRow {
        FrameRow {
            frame,
            tic: 0,
            melt_step,
            probe_fb_hash: String::new(),
        }
    }

    #[test]
    fn a_melt_frame_needs_nothing_before_it_and_a_gameplay_frame_does() {
        // The committed probe fixture's own shape: the melt's first and
        // last frames, then the frames after it.
        assert!(contiguous("nat", &[row(0, 1), row(39, 41), row(40, 0), row(41, 0)]).is_ok());
        let error = contiguous("nat", &[row(0, 1), row(39, 41), row(40, 0), row(1000, 0)])
            .expect_err("frame 1000 has nothing to draw over");
        assert!(error.to_string().contains("frame 1000"), "{error}");
        assert!(error.to_string().contains("frame 999"), "{error}");
    }

    #[test]
    fn a_frame_past_the_end_says_where_the_run_ends() {
        let error = Error::NoSuchFrame {
            database: "nat".to_owned(),
            frame: 5000,
            last: 2171,
        };
        let message = error.to_string();
        assert!(message.contains("5000"), "{message}");
        assert!(message.contains("2171"), "{message}");
    }
}
