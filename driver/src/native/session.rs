//! The two resident statements of one native-mode session, driven together.
//!
//! The simulation writes one row per tic into `native_state`, the renderer
//! one row per frame into `native_frames`, and the renderer reads the state
//! row the simulation just wrote. So the order is fixed: feed a tic, wait
//! for it, feed the frame that reads it. The statement text belongs to
//! whoever generates the SQL; this drives whatever it is given.
//!
//! A session opens the components it needs. With no simulation,
//! `native_state` holds rows something else wrote, the reference emulator's
//! probe among them, and [`Session::wait_sim`] reads them the same way.
//! With no renderer, a run drives tics and looks at the state rows rather
//! than at frames. Feeding a component a session did not open is an error
//! rather than a row that goes nowhere.
//!
//! A statement the server has abandoned goes on taking rows without
//! committing them, so a session finds out from [`Session::wait_sim`]
//! timing out. [`Session::recover`] is what follows: it ends both
//! statements, reports what each said, opens them again and gives back the
//! tic to resume from.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration; // purity-ok: a timeout and a measured wait in the driver loop, never a value a statement reads

use bytes::Bytes;
use clickhouse::Row;
use serde::Deserialize;
use tokio::time::Instant; // purity-ok: pacing and timeouts in the driver loop, never a value a statement reads

use super::rowbinary;
use super::settings::resident_settings;
use super::stream::{CLOSE_TIMEOUT, Resident, ResidentError};
use crate::checkpoint::hex64;
use crate::client::{self, ConnArgs, Db};

/// The columns the simulation statement reads, in wire order. `pad` carries
/// the padding row the transport writes behind the statement.
pub const SIM_INPUT_SCHEMA: &str =
    "tic UInt32, source UInt8, keys UInt32, mouse_dx Int16, mouse_dy Int16, pad String";

/// The columns the renderer statement reads, in wire order.
pub const RENDER_INPUT_SCHEMA: &str = "frame UInt32, tic UInt32, melt_step UInt8, pad String";

/// The table the simulation writes, keyed by tic.
pub const STATE_TABLE: &str = "native_state";

/// The table the renderer writes, keyed by frame.
pub const FRAMES_TABLE: &str = "native_frames";

/// How long a paced run waits for one tic of a warm statement before it
/// calls the statement dead. A tic's budget is 28.6 ms, so this is a wide
/// margin over the slowest tic and not a target. The first tic of a session
/// is not one of these: it pays for the statement's analysis, and a caller
/// passes its own budget for that.
pub const TIC_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a wait pauses between polls. The poll is a query round trip,
/// which paces the loop on its own; this keeps a slow tic from turning into
/// a tight query loop.
const POLL_SLEEP: Duration = Duration::from_micros(250);

/// Distinguishes the query ids handed out in one process.
static QUERY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Anything that stops a session from running a tic.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("the simulation statement: {source}")]
    Sim {
        #[source]
        source: ResidentError,
    },
    #[error("the renderer statement: {source}")]
    Render {
        #[source]
        source: ResidentError,
    },
    #[error("reading {database}.{table}: {source}")]
    Read {
        database: String,
        table: String,
        #[source]
        source: client::Error,
    },
    #[error(
        "tic {tic} was not written within {waited:?}. The simulation statement \
         has stopped; recover the session and resume from the last committed tic"
    )]
    TicTimeout { tic: u32, waited: Duration },
    #[error(
        "this session opened the renderer alone, so it has no simulation to \
         feed tic {tic} to. Its state rows come from whatever wrote \
         {database}.{STATE_TABLE}"
    )]
    NoSim { database: String, tic: u32 },
    #[error(
        "this session opened no renderer, so it has no statement to feed \
         frame {frame} to"
    )]
    NoRender { frame: u32 },
    #[error(
        "frame {frame} was not written within {waited:?}. The renderer \
         statement has stopped; recover the session and feed the frame again"
    )]
    FrameTimeout { frame: u32, waited: Duration },
}

/// One frame as `native_frames` holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub frame: u32,
    /// 320x200 8bpp pixels, row-major.
    pub fb: Bytes,
    /// The palette the status bar chose.
    pub palette: Bytes,
    /// The two rendered to RGB.
    pub rgb32: Bytes,
    /// `xxHash64(fb || palette)` as 16 lowercase hex digits.
    pub fb_hash: String,
}

/// One frame, with what it cost to get it.
#[derive(Debug)]
pub struct Waited {
    pub frame: Frame,
    /// From the row being sent to the frame being readable.
    pub waited: Duration,
    /// How long the poll that found it took, which is the round trip that
    /// carries the frame's bytes back.
    pub read: Duration,
}

/// What one call to [`Session::recover`] found.
#[derive(Debug)]
pub struct Recovery {
    /// The first tic not committed, which is where the session resumes.
    pub resume_tic: u32,
    /// What the simulation statement reported when it was ended, if it
    /// had failed.
    pub sim: Option<ResidentError>,
    /// The same for the renderer.
    pub render: Option<ResidentError>,
}

/// The columns `poll_frame` reads back, in one row.
#[derive(Row, Deserialize)]
struct FrameRow {
    /// `NULL` when the frame has not been written.
    fb_hash: Option<String>,
    fb: Bytes,
    palette: Bytes,
    rgb32: Bytes,
}

/// Both statements of one session, plus the connection that reads their
/// output back.
pub struct Session {
    database: String,
    sim_statement: Option<String>,
    render_statement: Option<String>,
    sim: Option<Resident>,
    render: Option<Resident>,
    sim_query_id: String,
    render_query_id: String,
    db: Db,
}

impl Session {
    /// Opens the statements against `database`.
    ///
    /// `sim_statement` is `None` for a session that renders from state rows
    /// already in the database, and `render_statement` is `None` for one
    /// that runs tics and looks at what they wrote. The statements are
    /// kept, because recovery reopens them unchanged. Each runs under its
    /// own `query_id`, so it can be found in `system.query_log` and killed
    /// by name.
    pub async fn open(
        conn: &ConnArgs,
        database: &str,
        sim_statement: Option<&str>,
        render_statement: Option<&str>,
    ) -> Result<Session, SessionError> {
        let mut at = conn.clone();
        at.database = database.to_owned();
        let sim_query_id = query_id(database, "sim");
        let render_query_id = query_id(database, "render");
        let sim = match sim_statement {
            Some(statement) => Some(
                open_one(&at, statement, SIM_INPUT_SCHEMA, &sim_query_id)
                    .await
                    .map_err(|source| SessionError::Sim { source })?,
            ),
            None => None,
        };
        let render = match render_statement {
            Some(statement) => Some(
                open_one(&at, statement, RENDER_INPUT_SCHEMA, &render_query_id)
                    .await
                    .map_err(|source| SessionError::Render { source })?,
            ),
            None => None,
        };
        Ok(Session {
            database: database.to_owned(),
            sim_statement: sim_statement.map(str::to_owned),
            render_statement: render_statement.map(str::to_owned),
            sim,
            render,
            sim_query_id,
            render_query_id,
            db: at.connect_uncompressed(),
        })
    }

    /// Whether this session drives a simulation of its own.
    pub fn has_sim(&self) -> bool {
        self.sim_statement.is_some()
    }

    /// Whether this session drives a renderer of its own.
    pub fn has_render(&self) -> bool {
        self.render_statement.is_some()
    }

    /// The `query_id` the simulation statement runs under. A fresh one is
    /// taken on every [`recover`](Session::recover).
    pub fn sim_query_id(&self) -> &str {
        &self.sim_query_id
    }

    /// The `query_id` the renderer statement runs under.
    pub fn render_query_id(&self) -> &str {
        &self.render_query_id
    }

    /// Sends the input row for one tic. `source` 0 takes the tic command
    /// from the demo lump, 1 builds it from `keys` and the mouse deltas.
    ///
    /// A session that opened the renderer alone has nothing to send it to.
    pub fn feed_sim(
        &self,
        tic: u32,
        source: u8,
        keys: u32,
        mouse_dx: i16,
        mouse_dy: i16,
    ) -> Result<(), SessionError> {
        if !self.has_sim() {
            return Err(SessionError::NoSim {
                database: self.database.clone(),
                tic,
            });
        }
        let mut row = rowbinary::Row::with_capacity(16);
        row.u32(tic)
            .u8(source)
            .u32(keys)
            .i16(mouse_dx)
            .i16(mouse_dy)
            .bytes(b"");
        self.statement(Role::Sim)?
            .send(row.finish())
            .map_err(|source| SessionError::Sim { source })
    }

    /// Waits for the simulation to write `tic`, and returns how long that
    /// took. The caller sends the next tic only after this returns.
    ///
    /// The budget is the caller's, as it is for
    /// [`wait_frame`](Session::wait_frame) and for the same reason: the
    /// first tic of a session pays for the statement's analysis, which is
    /// seconds, and every tic after it is milliseconds.
    /// [`TIC_TIMEOUT`] is the one a paced run uses once the statement is
    /// warm.
    pub async fn wait_sim(&self, tic: u32, timeout: Duration) -> Result<Duration, SessionError> {
        let started = Instant::now(); // purity-ok: measuring what this call waits, see the import
        loop {
            if self.committed_tic().await? >= tic {
                return Ok(started.elapsed());
            }
            let waited = started.elapsed();
            if waited >= timeout {
                return Err(SessionError::TicTimeout { tic, waited });
            }
            tokio::time::sleep(POLL_SLEEP).await;
        }
    }

    /// Sends the input row for one frame. `melt_step` drives the screen
    /// wipe.
    pub fn feed_render(&self, frame: u32, tic: u32, melt_step: u8) -> Result<(), SessionError> {
        if !self.has_render() {
            return Err(SessionError::NoRender { frame });
        }
        let mut row = rowbinary::Row::with_capacity(12);
        row.u32(frame).u32(tic).u8(melt_step).bytes(b"");
        self.statement(Role::Render)?
            .send(row.finish())
            .map_err(|source| SessionError::Render { source })
    }

    /// Reads one frame if the renderer has written it, in one query.
    ///
    /// The read is retried on a fresh connection when the pooled one it
    /// went out on had been closed by the server, which
    /// [`Db::fetch_one_reconnecting`] decides.
    pub async fn poll_frame(&self, frame: u32) -> Result<Option<Frame>, SessionError> {
        let table = format!("{}.{FRAMES_TABLE}", self.database);
        let sql = format!(
            "SELECT {} AS fb_hash, \
                    joinGet('{table}', 'fb', toUInt32({frame})) AS fb, \
                    joinGet('{table}', 'palette', toUInt32({frame})) AS palette, \
                    joinGet('{table}', 'rgb32', toUInt32({frame})) AS rgb32",
            hex64(&format!(
                "joinGetOrNull('{table}', 'fb_hash', toUInt32({frame}))"
            ))
        );
        let row = self
            .db
            .fetch_one_reconnecting::<FrameRow>(&sql)
            .await
            .map_err(|source| self.read_error(FRAMES_TABLE, source))?;
        Ok(row.fb_hash.map(|fb_hash| Frame {
            frame,
            fb: row.fb,
            palette: row.palette,
            rgb32: row.rgb32,
            fb_hash,
        }))
    }

    /// Waits for the renderer to write `frame`, and returns it with how
    /// long that took.
    ///
    /// The budget is the caller's, because what a frame may take depends on
    /// what the caller is doing: a paced run has a tic, a one-off render has
    /// as long as it needs.
    pub async fn wait_frame(&self, frame: u32, timeout: Duration) -> Result<Waited, SessionError> {
        let started = Instant::now(); // purity-ok: measuring what this call waits, see the import
        loop {
            let polled = Instant::now(); // purity-ok: measuring one read-back, see the import
            let found = self.poll_frame(frame).await?;
            let read = polled.elapsed();
            if let Some(frame) = found {
                return Ok(Waited {
                    frame,
                    waited: started.elapsed(),
                    read,
                });
            }
            let waited = started.elapsed();
            if waited >= timeout {
                return Err(SessionError::FrameTimeout { frame, waited });
            }
            tokio::time::sleep(POLL_SLEEP).await;
        }
    }

    /// The tic a resumed session starts from: one past the highest tic
    /// `native_state` holds, and 1 when it holds none.
    pub async fn resume_point(&self) -> Result<u32, SessionError> {
        Ok(self.committed_tic().await? + 1)
    }

    /// Ends the statements and opens them again.
    ///
    /// Both go, not just the one that looks dead: a failed statement takes
    /// rows without committing them, so which one stopped is not something
    /// a caller can read off. The errors each reported are in the
    /// [`Recovery`], along with the tic to resume from. Each is given
    /// [`CLOSE_TIMEOUT`] to answer.
    pub async fn recover(&mut self, conn: &ConnArgs) -> Result<Recovery, SessionError> {
        let mut at = conn.clone();
        at.database = self.database.clone();

        let sim = end(self.sim.take()).await;
        let render = end(self.render.take()).await;

        self.sim_query_id = query_id(&self.database, "sim");
        self.render_query_id = query_id(&self.database, "render");
        self.sim = match &self.sim_statement {
            Some(statement) => Some(
                open_one(&at, statement, SIM_INPUT_SCHEMA, &self.sim_query_id)
                    .await
                    .map_err(|source| SessionError::Sim { source })?,
            ),
            None => None,
        };
        self.render = match &self.render_statement {
            Some(statement) => Some(
                open_one(&at, statement, RENDER_INPUT_SCHEMA, &self.render_query_id)
                    .await
                    .map_err(|source| SessionError::Render { source })?,
            ),
            None => None,
        };

        Ok(Recovery {
            resume_tic: self.resume_point().await?,
            sim,
            render,
        })
    }

    /// Ends both statements and reports what each said, giving each
    /// [`CLOSE_TIMEOUT`] to answer.
    pub async fn close(mut self) -> Result<(), SessionError> {
        let sim = end(self.sim.take()).await;
        let render = end(self.render.take()).await;
        match (sim, render) {
            (Some(source), _) => Err(SessionError::Sim { source }),
            (None, Some(source)) => Err(SessionError::Render { source }),
            (None, None) => Ok(()),
        }
    }

    /// The highest tic `native_state` holds, 0 when it holds none. The
    /// query reads the key column alone, so it does not touch the state
    /// rows themselves.
    ///
    /// Retried the same way [`Session::poll_frame`] is.
    async fn committed_tic(&self) -> Result<u32, SessionError> {
        let sql = format!("SELECT max(tic) FROM {}.{STATE_TABLE}", self.database);
        self.db
            .fetch_one_reconnecting::<u32>(&sql)
            .await
            .map_err(|source| self.read_error(STATE_TABLE, source))
    }

    fn read_error(&self, table: &str, source: client::Error) -> SessionError {
        SessionError::Read {
            database: self.database.clone(),
            table: table.to_owned(),
            source,
        }
    }

    fn statement(&self, role: Role) -> Result<&Resident, SessionError> {
        let (slot, wrap): (&Option<Resident>, fn(ResidentError) -> SessionError) = match role {
            Role::Sim => (&self.sim, |source| SessionError::Sim { source }),
            Role::Render => (&self.render, |source| SessionError::Render { source }),
        };
        slot.as_ref().ok_or_else(|| {
            wrap(ResidentError::Ended {
                status: None,
                message: "the statement is closed; recover the session".to_owned(),
            })
        })
    }
}

/// Which of the two statements a call is about.
#[derive(Copy, Clone)]
enum Role {
    Sim,
    Render,
}

/// Opens one statement under `id`, with the settings a resident statement
/// needs.
async fn open_one(
    conn: &ConnArgs,
    statement: &str,
    input_schema: &str,
    id: &str,
) -> Result<Resident, ResidentError> {
    let mut settings = resident_settings(statement.len());
    settings.push(("query_id", id.to_owned()));
    Resident::open(conn, statement, input_schema, &settings).await
}

/// Ends a statement and keeps its error, if it had one.
async fn end(statement: Option<Resident>) -> Option<ResidentError> {
    match statement {
        Some(statement) => statement.close(CLOSE_TIMEOUT).await.err(),
        None => None,
    }
}

/// A `query_id` no other statement in this process shares.
fn query_id(database: &str, role: &str) -> String {
    let sequence = QUERY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{database}-{role}-{}-{sequence}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_input_schemas_carry_a_padding_column() {
        for schema in [SIM_INPUT_SCHEMA, RENDER_INPUT_SCHEMA] {
            rowbinary::padding_row(schema)
                .unwrap_or_else(|e| panic!("{schema} cannot carry a padding row: {e}"));
        }
    }

    #[test]
    fn a_query_id_names_its_database_role_and_process() {
        let first = query_id("clickdoom", "sim");
        let second = query_id("clickdoom", "sim");
        assert!(first.starts_with("clickdoom-sim-"), "{first}");
        assert_ne!(first, second, "two statements must not share an id");
    }
}
