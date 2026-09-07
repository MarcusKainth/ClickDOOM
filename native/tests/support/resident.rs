//! Driving the simulation's two resident statements the way a session
//! does, for a test that runs more than one tic.
//!
//! A batch that runs every tic through the first statement and then every
//! tic through the second is wrong past the first tic: the first
//! statement's own `previous(db)` reads the tic before out of
//! `native_state`, which the second statement writes, so a tic beyond the
//! first reads a row the second statement has not written yet if every tic
//! of the first runs before any tic of the second. This feeds one tic to
//! the first statement, waits for its own row in `native_stage`, feeds the
//! second from it, and waits for its own row in `native_state` before
//! feeding the next tic, opening each statement once rather than once per
//! tic.

use std::time::{Duration, Instant}; // purity-ok: pacing and timeouts in the test harness, never a value a statement reads

use bytes::Bytes;
use clickhouse::Row;
use serde::Deserialize;

use clickdoom_native::resident::{CLOSE_TIMEOUT, Endpoint, Resident, resident_settings, rowbinary};
use clickdoom_native::sql::render;
use clickdoom_native::sql::sim::tick::{self, Input};

use super::db::Fixture;

/// How long the first tic may take, which pays for every statement's own
/// analysis. Sized the way the driver's own `FIRST_TIC_TIMEOUT` is, for a
/// slow, shared machine rather than a target.
const FIRST_TIC_TIMEOUT: Duration = Duration::from_secs(300);

/// How long every tic after the first may take. A test has no 35 Hz budget
/// to keep, so this is wide rather than paced.
const TIC_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a wait polls.
const POLL_SLEEP: Duration = Duration::from_millis(1);

/// The renderer's own input schema. `native::sql::render::frame_transform`
/// generates a statement that reads the same columns through `input(...)`.
const RENDER_INPUT_SCHEMA: &str = "frame UInt32, tic UInt32, melt_step UInt8, pad String";

/// What one run left: the last tic fed, and how long the first tic took
/// against the mean of the rest, so a suite can print the analysis cost it
/// paid apart from the steady-state cost.
pub struct Ran {
    pub tic: u32,
    pub first: Duration,
    pub mean: Duration,
}

#[derive(Row, Deserialize)]
struct Present {
    present: u8,
}

/// Feeds `rows` through the simulation's two statements, one tic at a
/// time, the second statement's row for a tic fed only once the first
/// statement's own row for it is visible. Also drives the renderer, one
/// frame per tic, when `render` is set.
///
/// Opens every statement once against `fixture`'s own database and closes
/// them before returning. The fixture is `fixture`'s caller's: this reads
/// and writes its tables but never closes it.
pub async fn run(fixture: &Fixture, rows: &[Input], render: bool) -> Ran {
    let endpoint = super::db::endpoint(&fixture.database);
    let stage1_sql = tick::resident_statement_stage1(&fixture.database);
    let stage2_sql = tick::resident_statement_stage2(&fixture.database);
    let render_sql = render::frame_transform(&fixture.database);

    let (stage1, stage2, renderer) = tokio::join!(
        open(&endpoint, &stage1_sql, tick::INPUT_SCHEMA),
        open(&endpoint, &stage2_sql, tick::STAGE2_INPUT_SCHEMA),
        async {
            match render {
                true => Some(open(&endpoint, &render_sql, RENDER_INPUT_SCHEMA).await),
                false => None,
            }
        }
    );

    let mut first = Duration::ZERO;
    let mut rest = Duration::ZERO;
    let mut tic = 0;
    for (index, input) in rows.iter().enumerate() {
        let started = Instant::now(); // purity-ok: measuring what this call waits, see the import
        let timeout = match index {
            0 => FIRST_TIC_TIMEOUT,
            _ => TIC_TIMEOUT,
        };

        stage1
            .send(input_row(input))
            .unwrap_or_else(|err| panic!("feeding tic {}: {err}", input.tic));
        wait(timeout, || present(fixture, "native_stage", input.tic)).await;

        stage2.send(tic_row(input.tic)).unwrap_or_else(|err| {
            panic!("feeding tic {} to the second statement: {err}", input.tic)
        });
        wait(timeout, || present(fixture, "native_state", input.tic)).await;

        if let Some(renderer) = &renderer {
            renderer
                .send(render_row(input.tic))
                .unwrap_or_else(|err| panic!("feeding frame {}: {err}", input.tic));
            wait(timeout, || present(fixture, "native_frames", input.tic)).await;
        }

        let elapsed = started.elapsed();
        match index {
            0 => first = elapsed,
            _ => rest += elapsed,
        }
        tic = input.tic;
    }
    let mean = match rows.len() {
        0 | 1 => Duration::ZERO,
        n => rest / (n as u32 - 1),
    };

    close(stage1).await;
    close(stage2).await;
    if let Some(renderer) = renderer {
        close(renderer).await;
    }

    Ran { tic, first, mean }
}

async fn open(endpoint: &Endpoint, statement: &str, input_schema: &str) -> Resident {
    let settings = resident_settings(statement.len());
    Resident::open(endpoint, statement, input_schema, &settings)
        .await
        .unwrap_or_else(|err| panic!("opening a resident statement: {err}"))
}

async fn close(resident: Resident) {
    resident
        .close(CLOSE_TIMEOUT)
        .await
        .unwrap_or_else(|err| panic!("closing a resident statement: {err}"));
}

fn input_row(input: &Input) -> Bytes {
    let mut row = rowbinary::Row::with_capacity(16);
    row.u32(input.tic)
        .u8(input.source)
        .u32(input.keys)
        .i16(input.mouse.0)
        .i16(input.mouse.1)
        .bytes(b"");
    row.finish()
}

fn tic_row(tic: u32) -> Bytes {
    let mut row = rowbinary::Row::with_capacity(8);
    row.u32(tic).bytes(b"");
    row.finish()
}

fn render_row(tic: u32) -> Bytes {
    let mut row = rowbinary::Row::with_capacity(12);
    row.u32(tic).u32(tic).u8(0).bytes(b"");
    row.finish()
}

/// Whether `table` holds a row for `tic` yet, read through a real column
/// rather than the key, the way the driver's own presence checks do.
async fn present(fixture: &Fixture, table: &str, tic: u32) -> bool {
    let column = match table {
        "native_frames" => "fb_hash",
        _ => "leveltime",
    };
    let sql = format!(
        "SELECT toUInt8(joinGetOrNull('{}.{table}', '{column}', toUInt32({tic})) IS NOT NULL) \
         AS present",
        fixture.database
    );
    fixture.scalar::<Present>(&sql).await.present != 0
}

async fn wait<F, Fut>(timeout: Duration, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let started = Instant::now(); // purity-ok: measuring what this call waits, see the import
    loop {
        if check().await {
            return;
        }
        let waited = started.elapsed();
        if waited >= timeout {
            panic!("nothing landed within {timeout:?}");
        }
        tokio::time::sleep(POLL_SLEEP).await;
    }
}
