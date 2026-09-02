//! The level's first state row, and the differential that reads it, against
//! a real ClickHouse server.
//!
//! The spawn is checked against `support::spawn`, which reads the WAD a
//! second time from the engine's own source rather than from the SQL.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::{parity, probe, sim};
use clickdoom_native::{load, sql, wad::Wad};
use clickdoom_spec::native_state;
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::spawn;

/// The skill `DEMO3` plays at, which is what the SQL reads from the demo
/// header and the oracle has to be told.
const SKILL: i32 = 2;

/// `p_mobj.h`
const MF_SPAWNCEILING: i32 = 256;

/// Loads the WAD, decodes `E1M7` and spawns it once, then runs every check
/// against the one database.
#[tokio::test]
async fn the_first_state_row_is_the_level_the_wad_describes() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_setup").await;
    let mut plan = load::plan(&fixture.database, &wad);
    plan.extend(sql::level_statements(
        &fixture.database,
        support::MAP,
        support::DEMO,
    ));
    plan.push(probe::schema_statement(&fixture.database));
    plan.extend(sim::load_statements(&fixture.database));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let level = spawn::level(&wad, support::MAP, SKILL);
    the_counts_are_what_the_map_holds(&fixture, &level).await;
    every_mobj_is_the_thing_the_map_lists(&fixture, &level).await;
    every_mobj_stands_on_its_own_sector(&fixture).await;
    every_light_thinker_drew_in_sector_order(&fixture, &level).await;
    the_differential_reports_what_the_probe_disagrees_with(&fixture).await;

    fixture.finish().await;
}

#[derive(Row, Deserialize)]
struct Counts {
    mobjs: u64,
    thinkers: u64,
    prndindex: u8,
    rndindex: u8,
    next_seq: u32,
    next_linkseq: u32,
    totalkills: i32,
    totalitems: i32,
    totalsecret: i32,
    p_mo: u32,
    unresolved: u8,
    unimplemented: u64,
}

/// `G_InitNew` clears the random index and every draw the setup makes moves
/// it on, so the index the row carries is the number of calls `P_SpawnMobj`,
/// `P_SpawnMapThing` and `P_SpawnSpecials` made between them.
async fn the_counts_are_what_the_map_holds(fixture: &Fixture, level: &spawn::Level) {
    let row: Counts = fixture
        .scalar(&format!(
            "SELECT length(m_x) AS mobjs, length(s_kind) AS thinkers, prndindex, rndindex, \
             next_seq, next_linkseq, totalkills, totalitems, totalsecret, p_mo, \
             unresolved, unimplemented \
             FROM {}.native_state WHERE tic = 0",
            fixture.database
        ))
        .await;
    assert_eq!(row.mobjs as usize, level.mobjs.len(), "mobjs");
    assert_eq!(row.thinkers as usize, level.thinkers.len(), "thinkers");
    assert_eq!(u32::from(row.prndindex), level.draws & 0xff, "prndindex");
    assert_eq!(row.rndindex, 0, "M_ClearRandom leaves the menu index at 0");
    assert_eq!(
        row.next_seq as usize,
        level.mobjs.len() + level.thinkers.len() + 1
    );
    assert_eq!(row.next_linkseq as usize, level.mobjs.len() + 1);
    assert_eq!(row.totalkills, level.totalkills);
    assert_eq!(row.totalitems, level.totalitems);
    assert_eq!(row.totalsecret, level.totalsecret);
    let player = level.mobjs.iter().position(|m| m.player == 0).unwrap();
    assert_eq!(row.p_mo as usize, player + 1, "the player's slot");
    assert_eq!(row.unresolved, 0);
    assert_eq!(row.unimplemented, 0, "E1M7 reaches no unwritten path");
}

#[derive(Row, Deserialize)]
struct Mobjs {
    m_x: Vec<i32>,
    m_y: Vec<i32>,
    m_angle: Vec<u32>,
    m_type: Vec<i32>,
    m_tics: Vec<i32>,
    m_state: Vec<i32>,
    m_health: Vec<i32>,
    m_radius: Vec<i32>,
    m_height: Vec<i32>,
    m_flags: Vec<i32>,
    m_reactiontime: Vec<i32>,
    m_lastlook: Vec<i32>,
    m_player: Vec<i8>,
    m_subsector: Vec<i32>,
    m_sp_x: Vec<i16>,
    m_sp_y: Vec<i16>,
    m_sp_angle: Vec<i16>,
    m_sp_type: Vec<i16>,
    m_sp_options: Vec<i16>,
}

/// Slot by slot, every field `P_SpawnMobj` and `P_SpawnMapThing` write.
///
/// `lastlook` and `tics` come from the random table, so they only agree
/// when both sides made the same number of draws in the same order.
async fn every_mobj_is_the_thing_the_map_lists(fixture: &Fixture, level: &spawn::Level) {
    let ours: Mobjs = fixture
        .scalar(&format!(
            "SELECT m_x, m_y, m_angle, m_type, m_tics, m_state, m_health, m_radius, \
             m_height, m_flags, m_reactiontime, m_lastlook, m_player, m_subsector, \
             m_sp_x, m_sp_y, m_sp_angle, m_sp_type, m_sp_options \
             FROM {}.native_state WHERE tic = 0",
            fixture.database
        ))
        .await;
    for (at, want) in level.mobjs.iter().enumerate() {
        let slot = at + 1;
        let got = spawn::Mobj {
            x: ours.m_x[at],
            y: ours.m_y[at],
            angle: ours.m_angle[at],
            kind: ours.m_type[at],
            tics: ours.m_tics[at],
            state: ours.m_state[at],
            health: ours.m_health[at],
            radius: ours.m_radius[at],
            height: ours.m_height[at],
            flags: ours.m_flags[at],
            reactiontime: ours.m_reactiontime[at],
            lastlook: ours.m_lastlook[at],
            player: ours.m_player[at],
            subsector: ours.m_subsector[at],
            spawnpoint: [
                ours.m_sp_x[at],
                ours.m_sp_y[at],
                ours.m_sp_angle[at],
                ours.m_sp_type[at],
                ours.m_sp_options[at],
            ],
        };
        assert_eq!(&got, want, "mobj slot {slot}");
    }
}

#[derive(Row, Deserialize)]
struct Heights {
    wrong_floorz: u32,
    wrong_ceilingz: u32,
    wrong_z: u32,
}

/// `P_SetThingPosition` takes a mobj's floor and ceiling from the sector
/// its subsector belongs to, and `P_SpawnMapThing` drops it to one or the
/// other. The sectors come from the level tables rather than from the row,
/// so this checks the row against the map and not against itself.
async fn every_mobj_stands_on_its_own_sector(fixture: &Fixture) {
    let db = &fixture.database;
    let column = |table: &str, column: &str| {
        format!(
            "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((id, {column})))) \
             FROM {db}.{table})"
        )
    };
    let slots = "range(1, 1 + length(m_x))";
    let sector = "ss[1 + m_subsector[k]]";
    let row: Heights = fixture
        .scalar(&format!(
            "WITH {} AS ss, {} AS fh, {} AS ch \
             SELECT \
             toUInt32(arrayCount(k -> m_floorz[k] != fh[1 + {sector}], {slots})) AS wrong_floorz, \
             toUInt32(arrayCount(k -> m_ceilingz[k] != ch[1 + {sector}], {slots})) AS wrong_ceilingz, \
             toUInt32(arrayCount(k -> m_z[k] != if(bitAnd(m_flags[k], {MF_SPAWNCEILING}) != 0, \
             m_ceilingz[k] - m_height[k], m_floorz[k]), {slots})) AS wrong_z \
             FROM {db}.native_state WHERE tic = 0",
            column("lv_subsectors", "sector"),
            column("lv_sectors_static", "floorheight"),
            column("lv_sectors_static", "ceilingheight"),
        ))
        .await;
    assert_eq!(row.wrong_floorz, 0, "a mobj's floorz is not its sector's");
    assert_eq!(
        row.wrong_ceilingz, 0,
        "a mobj's ceilingz is not its sector's"
    );
    assert_eq!(
        row.wrong_z, 0,
        "a mobj did not drop to its floor or ceiling"
    );
}

#[derive(Row, Deserialize)]
struct Thinkers {
    s_sector: Vec<i32>,
    s_kind: Vec<u8>,
    s_count: Vec<i32>,
    s_mintime: Vec<i32>,
    s_maxtime: Vec<i32>,
    s_direction: Vec<i32>,
}

/// The light thinkers spawn after every thing has, so their counts only
/// agree when the things before them drew exactly as often as the engine.
async fn every_light_thinker_drew_in_sector_order(fixture: &Fixture, level: &spawn::Level) {
    let ours: Thinkers = fixture
        .scalar(&format!(
            "SELECT s_sector, s_kind, s_count, s_mintime, s_maxtime, s_direction \
             FROM {}.native_state WHERE tic = 0",
            fixture.database
        ))
        .await;
    for (at, want) in level.thinkers.iter().enumerate() {
        let got = spawn::Thinker {
            sector: ours.s_sector[at],
            kind: ours.s_kind[at],
            count: ours.s_count[at],
            mintime: ours.s_mintime[at],
            maxtime: ours.s_maxtime[at],
            direction: ours.s_direction[at],
        };
        assert_eq!(&got, want, "sector thinker slot {}", at + 1);
    }
}

#[derive(Row, Deserialize)]
struct Divergence {
    tic: u32,
    kind: String,
    slot: u32,
    field: String,
    ours: String,
    theirs: String,
}

#[derive(Row, Deserialize)]
struct FieldSummary {
    field: String,
    kind: String,
    tics: u64,
    first_tic: u32,
    slot: u32,
    ours: String,
    theirs: String,
}

/// The differential, against probe rows the test writes itself.
///
/// The first row repeats the state exactly, so nothing differs. The second
/// carries the same `gametic` with a later frame index and two fields
/// moved, which is the melt's shape: several frames share a tic and the
/// last of them is the state that tic left.
async fn the_differential_reports_what_the_probe_disagrees_with(fixture: &Fixture) {
    let db = &fixture.database;
    let agrees = probe_file(fixture, 0, &[]).await;
    fixture
        .execute(&[probe::insert(db, &agrees).unwrap()])
        .await
        .unwrap();
    let none: Vec<Divergence> = fixture.rows(&parity::first_divergence(db)).await;
    assert!(
        none.is_empty(),
        "a row that repeats the state diverges from it: {:?}",
        none.first().map(|d| d.field.clone())
    );

    let moved = probe_file(
        fixture,
        1,
        &[
            ("prndindex", "toString(prndindex + 1)"),
            (
                "m_x",
                "toString(arrayMap((v, i) -> if(i = 3, v + 1, v), m_x, arrayEnumerate(m_x)))",
            ),
        ],
    )
    .await;
    fixture
        .execute(&[probe::insert(db, &moved).unwrap()])
        .await
        .unwrap();

    let first: Vec<Divergence> = fixture.rows(&parity::first_divergence(db)).await;
    let first = first.first().expect("the moved fields are reported");
    assert_eq!(first.tic, 0);
    assert_eq!(first.field, "prndindex");
    assert_eq!(first.kind, "game");
    assert_eq!(first.slot, 0);
    assert_eq!(
        first.ours.parse::<i32>().unwrap() + 1,
        first.theirs.parse::<i32>().unwrap()
    );

    let summary: Vec<FieldSummary> = fixture.rows(&parity::field_summary(db)).await;
    let fields: Vec<&str> = summary.iter().map(|row| row.field.as_str()).collect();
    assert_eq!(fields, ["prndindex", "m_x"], "{summary:?}");
    let m_x = &summary[1];
    assert_eq!(m_x.kind, "mobj");
    assert_eq!(m_x.slot, 3, "the slot that moved");
    assert_eq!(m_x.tics, 1);
    assert_eq!(m_x.first_tic, 0);
    assert_eq!(
        m_x.ours.parse::<i32>().unwrap() + 1,
        m_x.theirs.parse::<i32>().unwrap()
    );
}

impl std::fmt::Debug for FieldSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} tic {} slot {}: {} vs {}",
            self.kind, self.field, self.first_tic, self.slot, self.ours, self.theirs
        )
    }
}

/// A probe file carrying tic 0 as one frame, with `moved` replacing the
/// expression a named column is written from.
///
/// The header is `probe::names()`, which is what the loader checks against,
/// so a file this builds is the shape the reference emulator writes.
async fn probe_file(fixture: &Fixture, frame: u32, moved: &[(&str, &str)]) -> String {
    let cells: Vec<String> = probe::names()
        .iter()
        .map(|name| match *name {
            "frame_index" => frame.to_string(),
            "gametic" => "toString(tic)".to_owned(),
            "fb_hash" => "'0000000000000000'".to_owned(),
            name => moved
                .iter()
                .find(|(column, _)| *column == name)
                .map_or_else(
                    || format!("toString({name})"),
                    |(_, expr)| (*expr).to_owned(),
                ),
        })
        .collect();
    let line: String = fixture
        .scalar(&format!(
            "SELECT arrayStringConcat([{}], '\t') FROM {}.native_state WHERE tic = 0",
            cells.join(", "),
            fixture.database
        ))
        .await;
    format!(
        "# refemu-probe 1\n# state_schema_version\t{}\n# columns\t{}\n{line}\n",
        native_state::STATE_SCHEMA_VERSION,
        probe::names().join("\t")
    )
}
