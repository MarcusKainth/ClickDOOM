//! The three resident statements of one native-mode session, driven
//! together.
//!
//! The simulation is two statements chained through `native_stage`: the
//! first writes one row per tic there, through the player and the
//! thinkers, and the second reads that same row back and writes
//! `native_state`, through the specials and `G_Ticker`. [`Session::open`]
//! sends both at once, so their analyses overlap rather than adding up,
//! and [`Session::wait_sim`] feeds the second the moment the first's own
//! row is there, so a caller that only calls [`Session::feed_sim`] and
//! [`Session::wait_sim`] sees no difference from one statement. The
//! renderer writes one row per frame into `native_frames`, and reads the
//! state row the simulation just wrote. So the order is fixed: feed a tic,
//! wait for it, feed the frame that reads it. The statement text belongs
//! to whoever generates the SQL; this drives whatever it is given.
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
//! timing out. [`Session::recover`] is what follows: it ends every
//! statement, reports what each said, opens them again and gives back the
//! tic to resume from. Both simulation statements restart from the same
//! tic, since a `native_stage` row a recovered run cannot show was ever
//! read is not one to trust.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration; // purity-ok: a timeout and a measured wait in the driver loop, never a value a statement reads

use bytes::Bytes;
use clickhouse::Row;
use serde::Deserialize;
use tokio::time::Instant; // purity-ok: pacing and timeouts in the driver loop, never a value a statement reads

use clickdoom_native::resident::{
    CLOSE_TIMEOUT, Endpoint, Resident, ResidentError, resident_settings, rowbinary,
};

use crate::checkpoint::hex64;
use crate::client::{self, ConnArgs, Db};

/// The columns the simulation's first statement reads, in wire order.
/// `pad` carries the padding row the transport writes behind the
/// statement.
pub const SIM_INPUT_SCHEMA: &str =
    "tic UInt32, source UInt8, keys UInt32, mouse_dx Int16, mouse_dy Int16, pad String";

/// The columns the simulation's second statement reads: a tic number
/// alone, since everything else it needs is already in the row the first
/// statement left for that tic.
pub const SIM_STAGE2_INPUT_SCHEMA: &str = "tic UInt32, pad String";

/// The columns the renderer statement reads, in wire order.
pub const RENDER_INPUT_SCHEMA: &str = "frame UInt32, tic UInt32, melt_step UInt8, pad String";

/// The table the simulation's first statement writes, keyed by tic, and
/// the second reads back the same way `native_state` reads the tic before.
pub const STAGE_TABLE: &str = "native_stage";

/// The table the simulation's second statement writes, keyed by tic.
pub const STATE_TABLE: &str = "native_state";

/// The table the renderer writes, keyed by frame.
pub const FRAMES_TABLE: &str = "native_frames";

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
    /// The same, but naming which of the simulation's own two statements:
    /// [`Session::close`] ends both, so a caller debugging one that hangs
    /// needs to know which. `diagnostic` carries what `system.processes`
    /// and `system.query_log` say about that statement's own query id when
    /// the close timed out, empty otherwise.
    #[error("the simulation's own {stage} statement: {source}{diagnostic}")]
    SimClose {
        stage: &'static str,
        #[source]
        source: ResidentError,
        diagnostic: Diagnostic,
    },
    #[error("the renderer statement: {source}")]
    Render {
        #[source]
        source: ResidentError,
    },
    /// The same, but for [`Session::close`] specifically, carrying the same
    /// diagnostic a timed-out close reads for the simulation.
    #[error("the renderer statement, closing: {source}{diagnostic}")]
    RenderClose {
        #[source]
        source: ResidentError,
        diagnostic: Diagnostic,
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
    #[error("emptying {database}.{STAGE_TABLE}: {source}")]
    Reset {
        database: String,
        #[source]
        source: client::Error,
    },
}

/// What a timed-out close found reading `system.processes` and
/// `system.query_log` for the statement's own query id, for
/// [`SessionError::SimClose`] and [`SessionError::RenderClose`]. Empty for
/// every other error, and for a close that answered before
/// [`CLOSE_TIMEOUT`].
#[derive(Debug)]
pub struct Diagnostic(Option<String>);

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(text) => write!(f, " ({text})"),
            None => Ok(()),
        }
    }
}

/// One row of `system.processes`, for a statement still running when its
/// close timed out.
#[derive(Row, Deserialize)]
struct ProcessRow {
    elapsed: f64,
}

/// One row of `system.query_log`, for a statement that had already
/// finished when its close timed out.
#[derive(Row, Deserialize)]
struct QueryLogRow {
    query_duration_ms: u64,
    exception: String,
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
    /// Set when the state row this frame draws from left `unresolved` or
    /// `unimplemented`, read in the same poll as the frame itself.
    pub refusal: Option<super::Refusal>,
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

/// One tic run, with how long it took, whether `native_state` left it
/// unresolved or unimplemented, and whether it ran past the demo lump's
/// recorded commands, all read in the same poll that found it committed.
#[derive(Debug)]
pub struct Ran {
    pub elapsed: Duration,
    pub refusal: Option<super::Refusal>,
    pub demo_end: bool,
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
    /// The frame's own tic, read out of `native_frames` so the same query
    /// can look `native_state`'s refusal columns up by it.
    tic: u32,
    unresolved: u64,
    unimplemented: u64,
}

/// One poll of whether `native_stage` holds a given tic. A `Join` engine
/// table refuses `joinGet` on its own key column, so this reads a
/// contract column back through `joinGetOrNull` instead, which answers
/// `NULL` for a key that is not there.
#[derive(Row, Deserialize)]
struct StagedRow {
    present: u8,
}

/// The columns a poll of `native_state`'s highest committed tic reads
/// back, in one row.
#[derive(Row, Deserialize)]
struct CommittedRow {
    tic: u32,
    unresolved: u64,
    unimplemented: u64,
    demo_end: u8,
}

/// Every statement of one session, plus the connection that reads their
/// output back.
pub struct Session {
    database: String,
    sim_statement: Option<(String, String)>,
    render_statement: Option<String>,
    sim: Option<Resident>,
    sim2: Option<Resident>,
    render: Option<Resident>,
    sim_query_id: String,
    sim2_query_id: String,
    render_query_id: String,
    db: Db,
}

impl Session {
    /// Opens the statements against `database`.
    ///
    /// `sim_statement` is the simulation's own two, first and second, and
    /// is `None` for a session that renders from state rows already in the
    /// database; `render_statement` is `None` for one that runs tics and
    /// looks at what they wrote. Every statement opens at once, not one
    /// after another, so a tic that pays for two analyses pays for the
    /// larger of them rather than their sum. The statements are kept,
    /// because recovery reopens them unchanged. Each runs under its own
    /// `query_id`, so it can be found in `system.query_log` and killed by
    /// name.
    pub async fn open(
        conn: &ConnArgs,
        database: &str,
        sim_statement: Option<(&str, &str)>,
        render_statement: Option<&str>,
    ) -> Result<Session, SessionError> {
        let mut at = conn.clone();
        at.database = database.to_owned();
        let db = at.connect_uncompressed();
        if sim_statement.is_some() {
            reset_stage(&db, database).await?;
        }
        let sim_query_id = query_id(database, "sim");
        let sim2_query_id = query_id(database, "sim2");
        let render_query_id = query_id(database, "render");

        let open_sim = async {
            match sim_statement {
                Some((stage1, _)) => {
                    Some(open_one(&at, stage1, SIM_INPUT_SCHEMA, &sim_query_id).await)
                }
                None => None,
            }
        };
        let open_sim2 = async {
            match sim_statement {
                Some((_, stage2)) => {
                    Some(open_one(&at, stage2, SIM_STAGE2_INPUT_SCHEMA, &sim2_query_id).await)
                }
                None => None,
            }
        };
        let open_render = async {
            match render_statement {
                Some(statement) => {
                    Some(open_one(&at, statement, RENDER_INPUT_SCHEMA, &render_query_id).await)
                }
                None => None,
            }
        };
        let (sim, sim2, render) = tokio::join!(open_sim, open_sim2, open_render);
        let sim = sim
            .transpose()
            .map_err(|source| SessionError::Sim { source })?;
        let sim2 = sim2
            .transpose()
            .map_err(|source| SessionError::Sim { source })?;
        let render = render
            .transpose()
            .map_err(|source| SessionError::Render { source })?;

        Ok(Session {
            database: database.to_owned(),
            sim_statement: sim_statement.map(|(a, b)| (a.to_owned(), b.to_owned())),
            render_statement: render_statement.map(str::to_owned),
            sim,
            sim2,
            render,
            sim_query_id,
            sim2_query_id,
            render_query_id,
            db,
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

    /// The `query_id` the simulation's first statement runs under. A fresh
    /// one is taken on every [`recover`](Session::recover).
    pub fn sim_query_id(&self) -> &str {
        &self.sim_query_id
    }

    /// The `query_id` the simulation's second statement runs under.
    pub fn sim2_query_id(&self) -> &str {
        &self.sim2_query_id
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
    /// took, along with whether `native_state` left `tic` unresolved or
    /// unimplemented. The caller sends the next tic only after this
    /// returns, and stops rather than feeding or drawing past a refusal.
    ///
    /// The two statements are one simulation to a caller: this feeds the
    /// second the moment `native_stage` holds the first's own row for
    /// `tic`, and only then goes on polling `native_state` for the tic to
    /// land there, so a caller that never looks past [`feed_sim`] and this
    /// sees no difference from a single statement.
    ///
    /// The budget is the caller's, as it is for
    /// [`wait_frame`](Session::wait_frame) and for the same reason: the
    /// first tic of a session pays for the statements' own analysis, the
    /// larger of the two since both are sent at once, which is seconds,
    /// and every tic after it is milliseconds.
    /// [`TIC_TIMEOUT`](clickdoom_native::resident::TIC_TIMEOUT) is the one
    /// a paced run uses once the statements are warm, and covers both hops.
    pub async fn wait_sim(&self, tic: u32, timeout: Duration) -> Result<Ran, SessionError> {
        let started = Instant::now(); // purity-ok: measuring what this call waits, see the import
        let mut fed_second = false;
        loop {
            if !fed_second && self.staged(tic).await? {
                self.feed_sim2(tic)?;
                fed_second = true;
            }
            let committed = self.committed().await?;
            if committed.tic >= tic {
                return Ok(Ran {
                    elapsed: started.elapsed(),
                    refusal: super::Refusal::at(
                        committed.tic,
                        committed.unresolved,
                        committed.unimplemented,
                    ),
                    demo_end: committed.demo_end != 0,
                });
            }
            let waited = started.elapsed();
            if waited >= timeout {
                return Err(SessionError::TicTimeout { tic, waited });
            }
            tokio::time::sleep(POLL_SLEEP).await;
        }
    }

    /// Whether `native_stage` holds the first statement's own row for
    /// `tic` yet.
    ///
    /// The table is empty whenever this is first asked: [`Session::open`]
    /// and [`Session::recover`] both truncate it before either simulation
    /// statement opens, so a row this call finds can only be the reopened
    /// first statement's own.
    async fn staged(&self, tic: u32) -> Result<bool, SessionError> {
        let table = format!("{}.{STAGE_TABLE}", self.database);
        let sql = format!(
            "SELECT toUInt8(joinGetOrNull('{table}', 'leveltime', toUInt32({tic})) \
             IS NOT NULL) AS present"
        );
        let row = self
            .db
            .fetch_one_reconnecting::<StagedRow>(&sql)
            .await
            .map_err(|source| self.read_error(STAGE_TABLE, source))?;
        Ok(row.present != 0)
    }

    /// Sends the input row that runs the second statement's own transform
    /// for `tic`, reading everything it needs back out of `native_stage`.
    fn feed_sim2(&self, tic: u32) -> Result<(), SessionError> {
        let mut row = rowbinary::Row::with_capacity(8);
        row.u32(tic).bytes(b"");
        self.statement(Role::Sim2)?
            .send(row.finish())
            .map_err(|source| SessionError::Sim { source })
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

    /// Reads one frame if the renderer has written it, in one query,
    /// together with whether `native_state` left the frame's own tic
    /// unresolved or unimplemented.
    ///
    /// The read is retried on a fresh connection when the pooled one it
    /// went out on had been closed by the server, which
    /// [`Db::fetch_one_reconnecting`] decides.
    pub async fn poll_frame(&self, frame: u32) -> Result<Option<Frame>, SessionError> {
        let frames = format!("{}.{FRAMES_TABLE}", self.database);
        let state = format!("{}.{STATE_TABLE}", self.database);
        let tic = format!("joinGet('{frames}', 'tic', toUInt32({frame}))");
        let sql = format!(
            "SELECT {} AS fb_hash, \
                    joinGet('{frames}', 'fb', toUInt32({frame})) AS fb, \
                    joinGet('{frames}', 'palette', toUInt32({frame})) AS palette, \
                    joinGet('{frames}', 'rgb32', toUInt32({frame})) AS rgb32, \
                    {tic} AS tic, \
                    joinGet('{state}', 'unresolved', {tic}) AS unresolved, \
                    joinGet('{state}', 'unimplemented', {tic}) AS unimplemented",
            hex64(&format!(
                "joinGetOrNull('{frames}', 'fb_hash', toUInt32({frame}))"
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
            refusal: super::Refusal::at(row.tic, row.unresolved, row.unimplemented),
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
        Ok(self.committed().await?.tic + 1)
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

        let (sim, sim2, render) = tokio::join!(
            end(self.sim.take()),
            end(self.sim2.take()),
            end(self.render.take())
        );
        let sim = sim.or(sim2);

        if self.sim_statement.is_some() {
            reset_stage(&self.db, &self.database).await?;
        }

        self.sim_query_id = query_id(&self.database, "sim");
        self.sim2_query_id = query_id(&self.database, "sim2");
        self.render_query_id = query_id(&self.database, "render");

        let open_sim = async {
            match &self.sim_statement {
                Some((stage1, _)) => {
                    Some(open_one(&at, stage1, SIM_INPUT_SCHEMA, &self.sim_query_id).await)
                }
                None => None,
            }
        };
        let open_sim2 = async {
            match &self.sim_statement {
                Some((_, stage2)) => {
                    Some(open_one(&at, stage2, SIM_STAGE2_INPUT_SCHEMA, &self.sim2_query_id).await)
                }
                None => None,
            }
        };
        let open_render = async {
            match &self.render_statement {
                Some(statement) => {
                    Some(open_one(&at, statement, RENDER_INPUT_SCHEMA, &self.render_query_id).await)
                }
                None => None,
            }
        };
        let (opened_sim, opened_sim2, opened_render) =
            tokio::join!(open_sim, open_sim2, open_render);
        self.sim = opened_sim
            .transpose()
            .map_err(|source| SessionError::Sim { source })?;
        self.sim2 = opened_sim2
            .transpose()
            .map_err(|source| SessionError::Sim { source })?;
        self.render = opened_render
            .transpose()
            .map_err(|source| SessionError::Render { source })?;

        Ok(Recovery {
            resume_tic: self.resume_point().await?,
            sim,
            render,
        })
    }

    /// Ends every statement and reports what each said, giving each
    /// [`CLOSE_TIMEOUT`] to answer.
    pub async fn close(mut self) -> Result<(), SessionError> {
        // Sequential, not concurrent: closing drops the sender and waits
        // for the server to drain the body and answer, and the second
        // statement's own body includes rows that read the first's, so
        // ending them at once risked the second waiting on a commit the
        // first's own close had not yet forced.
        if let Some(source) = end(self.sim.take()).await {
            end(self.sim2.take()).await;
            end(self.render.take()).await;
            let diagnostic = diagnose(&self.db, &source, &self.sim_query_id).await;
            return Err(SessionError::SimClose {
                stage: "first",
                source,
                diagnostic,
            });
        }
        if let Some(source) = end(self.sim2.take()).await {
            end(self.render.take()).await;
            let diagnostic = diagnose(&self.db, &source, &self.sim2_query_id).await;
            return Err(SessionError::SimClose {
                stage: "second",
                source,
                diagnostic,
            });
        }
        match end(self.render.take()).await {
            Some(source) => {
                let diagnostic = diagnose(&self.db, &source, &self.render_query_id).await;
                Err(SessionError::RenderClose { source, diagnostic })
            }
            None => Ok(()),
        }
    }

    /// The highest tic `native_state` holds, 0 when it holds none, with
    /// what that row left in `unresolved`, `unimplemented` and `demo_end`,
    /// in the one query.
    ///
    /// Retried the same way [`Session::poll_frame`] is.
    async fn committed(&self) -> Result<CommittedRow, SessionError> {
        let table = format!("{}.{STATE_TABLE}", self.database);
        let sql = format!(
            "SELECT tic, \
                    joinGet('{table}', 'unresolved', tic) AS unresolved, \
                    joinGet('{table}', 'unimplemented', tic) AS unimplemented, \
                    joinGet('{table}', 'demo_end', tic) AS demo_end \
             FROM (SELECT max(tic) AS tic FROM {table})"
        );
        self.db
            .fetch_one_reconnecting::<CommittedRow>(&sql)
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
            Role::Sim2 => (&self.sim2, |source| SessionError::Sim { source }),
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

/// Which statement a call is about.
#[derive(Copy, Clone)]
enum Role {
    Sim,
    Sim2,
    Render,
}

/// `conn` as the endpoint [`Resident::open`] takes: this module's own
/// connection arguments carry more than a resident statement needs to
/// know about.
pub(crate) fn endpoint(conn: &ConnArgs) -> Endpoint {
    Endpoint {
        host: conn.host.clone(),
        port: conn.port,
        user: conn.user.clone(),
        database: conn.database.clone(),
        password: conn.password.clone(),
    }
}

/// Empties `native_stage` before the simulation's first statement opens or
/// reopens. The table never carries anything past the tic in flight when its
/// own writer last ran, so a session pays for one recomputed tic rather than
/// risk a row a prior run left behind answering [`Session::staged`] for a tic
/// the reopened statement has not written itself.
async fn reset_stage(db: &Db, database: &str) -> Result<(), SessionError> {
    db.run(&format!(
        "TRUNCATE TABLE IF EXISTS {database}.{STAGE_TABLE}"
    ))
    .await
    .map_err(|source| SessionError::Reset {
        database: database.to_owned(),
        source,
    })
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
    Resident::open(&endpoint(conn), statement, input_schema, &settings).await
}

/// Ends a statement and keeps its error, if it had one.
async fn end(statement: Option<Resident>) -> Option<ResidentError> {
    match statement {
        Some(statement) => statement.close(CLOSE_TIMEOUT).await.err(),
        None => None,
    }
}

/// What `system.processes` and `system.query_log` say about `query_id`,
/// for a close that missed [`CLOSE_TIMEOUT`]. Empty for any other error,
/// since a statement that answered with its own error already said what
/// happened.
async fn diagnose(db: &Db, error: &ResidentError, query_id: &str) -> Diagnostic {
    if !matches!(error, ResidentError::Unanswered { .. }) {
        return Diagnostic(None);
    }
    let processes = format!("SELECT elapsed FROM system.processes WHERE query_id = '{query_id}'");
    match db.fetch_all::<ProcessRow>(&processes).await {
        Ok(rows) if !rows.is_empty() => {
            return Diagnostic(Some(format!(
                "system.processes still lists it, {:.1}s elapsed",
                rows[0].elapsed
            )));
        }
        Ok(_) => {}
        Err(source) => return Diagnostic(Some(format!("reading system.processes: {source}"))),
    }
    if let Err(source) = db.run("SYSTEM FLUSH LOGS").await {
        return Diagnostic(Some(format!("flushing system.query_log: {source}")));
    }
    let query_log = format!(
        "SELECT query_duration_ms, exception FROM system.query_log \
         WHERE query_id = '{query_id}' AND type != 'QueryStart' \
         ORDER BY event_time DESC LIMIT 1"
    );
    match db.fetch_one::<QueryLogRow>(&query_log).await {
        Ok(row) if row.exception.is_empty() => Diagnostic(Some(format!(
            "system.query_log shows it finished in {}ms with no exception",
            row.query_duration_ms
        ))),
        Ok(row) => Diagnostic(Some(format!(
            "system.query_log shows it finished in {}ms: {}",
            row.query_duration_ms, row.exception
        ))),
        Err(source) => Diagnostic(Some(format!(
            "not in system.processes, and not yet in system.query_log: {source}"
        ))),
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
    fn every_input_schema_carries_a_padding_column() {
        for schema in [
            SIM_INPUT_SCHEMA,
            SIM_STAGE2_INPUT_SCHEMA,
            RENDER_INPUT_SCHEMA,
        ] {
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
