//! `clickdoom native load`: an empty database in, a loaded one out.

use std::path::{Path, PathBuf};
use std::time::Duration; // purity-ok: summing the timings plan.rs measured, for the report line

use clap::Args;
use clickdoom_native::sql::{self, Statement};
use clickdoom_native::wad::Wad;

use crate::cli::{Exit, Failure, failed};
use crate::client::{ConnArgs, Db};
use crate::native::{melt, plan, probe};

/// The map `doom1.wad` ships a demo for, and the demo that plays it.
const MAP_DEFAULT: &str = "E1M7";
const DEMO_DEFAULT: &str = "DEMO3";

/// The sky texture episode one carries. The shareware WAD has only the one
/// episode, and which texture an episode uses is a name the caller gives.
const SKY_DEFAULT: &str = "SKY1";

#[derive(Args)]
#[command(
    about = "Decode a WAD into a database native mode can run against",
    // Hard-wrapped: clap only rewraps help text with its `wrap_help`
    // feature, which this binary does not enable.
    long_about = "\
Issue every statement a loaded level needs, in order: the schema, the WAD's
lumps, the engine's constant tables, the level decode, the renderer's own
tables, the simulation's first state row and the demo's melt schedule. The
driver issues them and times each phase. What the statements are, and what
order they go in inside a phase, belongs to the SQL that generates them.

Loading twice is loading once. Every table the schema declares is emptied
first, so a re-run replaces what is there rather than doubling it, and a
database holding something else keeps it. Pass --fresh to drop those tables
instead, which is what a changed schema needs.

--probe PATH is the other half: it loads the reference emulator's state rows
into probe_state and copies them into native_state, which is what `--from
probe` renders from. It leaves the level tables alone, so it runs after a
level load rather than instead of one.

Exit codes: 0 loaded, 1 a statement failed or a file could not be read."
)]
pub struct LoadCmd {
    #[command(flatten)]
    pub conn: ConnArgs,
    /// The WAD to decode
    #[arg(long, default_value = "rom/wad/doom1.wad")]
    pub wad: PathBuf,
    /// Map marker to load
    #[arg(long, default_value = MAP_DEFAULT)]
    pub map: String,
    /// Demo lump the simulation takes its tic commands from
    #[arg(long, default_value = DEMO_DEFAULT)]
    pub demo: String,
    /// Sky texture for the episode the map belongs to
    #[arg(long, default_value = SKY_DEFAULT)]
    pub sky: String,
    /// Drop the database before loading, rather than emptying its tables
    #[arg(long)]
    pub fresh: bool,
    /// Load this probe TSV into `probe_state` and `native_state` instead of
    /// loading a level
    #[arg(long, value_name = "PATH")]
    pub probe: Option<PathBuf>,
}

/// The database the connection itself sits on. Every statement a load
/// issues names its own database, and the database the flags name is what
/// the first statement creates, so the connection cannot be on it.
const CONNECT_TO: &str = "default";

pub(crate) async fn run(cmd: &LoadCmd) -> Result<Exit, Failure> {
    let mut at = cmd.conn.clone();
    at.database = CONNECT_TO.to_owned();
    let db = at.connect();
    match &cmd.probe {
        Some(path) => load_probe(cmd, &db, path).await,
        None => load_level(cmd, &db).await,
    }
}

/// The level, from the WAD's bytes to the renderer's tables.
async fn load_level(cmd: &LoadCmd, db: &Db) -> Result<Exit, Failure> {
    let database = &cmd.conn.database;
    let bytes = std::fs::read(&cmd.wad)
        .map_err(|err| failed(format!("reading {}: {err}", cmd.wad.display())))?;
    let wad = Wad::parse(&bytes).map_err(|err| {
        failed(format!(
            "{} is not a WAD this can read: {err}",
            cmd.wad.display()
        ))
    })?;
    let phases = phases(cmd, &wad).map_err(|err| failed(err.to_string()))?;

    println!(
        "{} into {}:{}/{database}: map {}, demo {}, sky {}",
        cmd.wad.display(),
        cmd.conn.host,
        cmd.conn.port,
        cmd.map,
        cmd.demo,
        cmd.sky
    );
    let reports = plan::run(db, &phases)
        .await
        .map_err(|err| failed(err.to_string()))?;
    report(&reports);
    Ok(Exit::Ok)
}

/// What the load issues: its own tables emptied, then the phases that fill
/// them.
fn phases(cmd: &LoadCmd, wad: &Wad<'_>) -> Result<Vec<plan::Phase>, melt::UnknownDemo> {
    let database = &cmd.conn.database;
    let mut phases = vec![plan::Phase::new("empty", empty(database, cmd.fresh))];
    phases.extend(plan::level_phases(
        database, wad, &cmd.map, &cmd.demo, &cmd.sky,
    )?);
    Ok(phases)
}

/// The reference emulator's state rows, for a run that renders from them.
async fn load_probe(cmd: &LoadCmd, db: &Db, path: &Path) -> Result<Exit, Failure> {
    let database = &cmd.conn.database;
    println!(
        "{} into {}:{}/{database}",
        path.display(),
        cmd.conn.host,
        cmd.conn.port
    );
    let loaded = probe::load(db, database, path)
        .await
        .map_err(|err| failed(err.to_string()))?;
    let (low, high) = loaded.frames;
    println!(
        "  {} {} rows, frames {low} to {high}\n  {} {} tics",
        probe::STAGING_TABLE,
        loaded.rows,
        probe::STATE_TABLE,
        loaded.tics
    );
    Ok(Exit::Ok)
}

/// What a load starts from, one statement per table the schema declares.
///
/// Only those tables, so a load into a database holding something else
/// leaves the something else alone. Emptying keeps the tables and drops
/// their rows, which is enough when the schema has not moved; `--fresh`
/// drops them, which is what a changed schema needs, because `CREATE TABLE
/// IF NOT EXISTS` leaves an existing table's columns alone.
fn empty(database: &str, fresh: bool) -> Vec<Statement> {
    let verb = match fresh {
        true => "DROP",
        false => "TRUNCATE",
    };
    sql::schema_tables()
        .into_iter()
        .map(|table| Statement::sql(format!("{verb} TABLE IF EXISTS {database}.{table}")))
        .collect()
}

/// `word`, with an `s` when there is not exactly one of the thing.
fn plural(count: usize, word: &str) -> String {
    match count {
        1 => word.to_owned(),
        _ => format!("{word}s"),
    }
}

fn report(reports: &[plan::PhaseReport]) {
    let mut total = Duration::ZERO;
    for phase in reports {
        total += phase.elapsed;
        let body = match phase.body_bytes {
            0 => String::new(),
            bytes => format!(", {bytes} bytes streamed"),
        };
        println!(
            "  {:<6} {:>3} {}{body} in {:.2} s",
            phase.name,
            phase.statements,
            plural(phase.statements, "statement"),
            phase.elapsed.as_secs_f64()
        );
    }
    println!("  loaded in {:.2} s", total.as_secs_f64());
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parsed(args: &[&str]) -> LoadCmd {
        #[derive(Parser)]
        struct Only {
            #[command(flatten)]
            load: LoadCmd,
        }
        let mut all = vec!["load"];
        all.extend_from_slice(args);
        Only::try_parse_from(all).expect("the arguments parse").load
    }

    fn names(cmd: &LoadCmd) -> Vec<&'static str> {
        let bytes = b"IWAD\x00\x00\x00\x00\x0c\x00\x00\x00".to_vec();
        let wad = Wad::parse(&bytes).expect("an empty WAD parses");
        phases(cmd, &wad)
            .expect("the default demo has a committed melt schedule")
            .iter()
            .map(|p| p.name)
            .collect()
    }

    #[test]
    fn the_database_is_emptied_before_the_schema_writes_to_it() {
        let cmd = parsed(&[]);
        let names = names(&cmd);
        assert_eq!(names[0], "empty");
        assert_eq!(names[1], "base");
    }

    #[test]
    fn the_level_decode_runs_after_the_lumps_and_before_the_renderers_tables() {
        let names = names(&parsed(&[]));
        let level = names
            .iter()
            .position(|n| *n == "level")
            .expect("a level phase");
        assert!(
            names
                .iter()
                .position(|n| *n == "base")
                .expect("a base phase")
                < level
        );
        assert!(
            names
                .iter()
                .position(|n| *n == "render")
                .expect("a render phase")
                > level
        );
    }

    /// Emptying runs before the schema, so the database may not exist yet.
    /// `IF EXISTS` covers a missing database as well as a missing table.
    #[test]
    fn emptying_names_every_table_the_schema_declares_and_nothing_else() {
        let truncated = empty("nat", false);
        assert_eq!(truncated.len(), sql::schema_tables().len());
        assert_eq!(
            truncated[0].sql, "TRUNCATE TABLE IF EXISTS nat.wad_lumps",
            "the schema's first table"
        );
        assert!(
            truncated
                .iter()
                .all(|s| s.sql.starts_with("TRUNCATE TABLE IF EXISTS nat.")),
            "a statement reaches past the schema's own tables"
        );
        assert!(
            empty("nat", true)
                .iter()
                .all(|s| s.sql.starts_with("DROP TABLE IF EXISTS nat.")),
        );
    }

    #[test]
    fn the_defaults_name_a_map_and_a_demo_the_shareware_wad_carries() {
        let cmd = parsed(&[]);
        assert_eq!(cmd.map, "E1M7");
        assert_eq!(cmd.demo, "DEMO3");
        assert_eq!(cmd.sky, "SKY1");
        assert!(melt::load_statements("nat", &cmd.demo).is_ok());
    }

    #[test]
    fn a_demo_with_no_committed_melt_schedule_fails_before_anything_is_issued() {
        let cmd = parsed(&["--demo", "DEMO1"]);
        assert!(melt::load_statements("nat", &cmd.demo).is_err());
    }
}
