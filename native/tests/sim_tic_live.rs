//! The tic's clocks, specials and tickers against a real ClickHouse server.
//!
//! Forty tics of `DEMO3` run through the same transform a session opens,
//! and each column this pull request computes is checked against a reader
//! written from the engine's own source rather than from the SQL.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::ticker;

/// How many tics the run covers. Long enough for the melt, the message
/// counter and the skull to have moved, short enough to keep the test
/// under a second.
const TICS: u32 = 40;

/// `f_wipe.c`: the melt draws one number per screen column, once.
const MELT_DRAWS: u32 = 320;
const MELT_TIC: u32 = 2;

/// `m_fixed.h`
const FRACUNIT: i32 = 1 << 16;

/// `p_spec.c`: the line special that scrolls its first side.
const SCROLL_LEFT: i16 = 48;

/// `hu_stuff.c`: how long a message stays up.
const MSGTIMEOUT: i32 = 4 * 35;

#[tokio::test]
async fn forty_tics_move_the_clocks_the_engine_moves() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_tic").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, TICS));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Tic> = fixture
        .rows(&format!(
            "SELECT tic, leveltime, rndindex, st_clock, st_randomnumber, st_faceindex, \
             st_facecount, st_priority, st_lastattackdown, st_oldhealth, st_lastcalc, \
             st_calc_oldhealth, p_attackdown, menu_skullanim, menu_whichskull, p_cmd_forwardmove, \
             p_cmd_sidemove, p_cmd_angleturn, p_cmd_buttons, demo_end, \
             texturetranslation, flattranslation, side_textureoffset \
             FROM {db}.native_state ORDER BY tic"
        ))
        .await;
    assert_eq!(
        rows.len() as u32,
        TICS + 1,
        "one row per tic and the setup's"
    );

    the_clocks_run_as_the_engine_runs_them(&rows);
    the_commands_are_the_demo_lump(&rows, &wad);
    the_face_follows_the_status_bar_s_ladder(&rows);
    let sides = sides(&fixture).await;
    the_scrolling_walls_move_one_unit_a_tic(&rows, &sides);
    let names = picture_names(&fixture).await;
    the_animated_pictures_cycle(&rows, &names);
    the_message_widget_takes_what_the_player_holds(&fixture).await;

    fixture.finish().await;
}

#[derive(Row, Deserialize)]
struct Message {
    hu_message: u64,
    hu_message_on: u8,
    hu_message_counter: i32,
    p_message: u64,
}

/// `HU_Ticker` against a message the run itself puts in the player's hand.
///
/// Nothing in the first forty tics gives the player a message, so the row
/// this writes is what stands in for the pickup that will.
async fn the_message_widget_takes_what_the_player_holds(fixture: &Fixture) {
    let db = &fixture.database;
    let held = 0x0123_4567_89ab_cdefu64;
    fixture
        .execute(&[sql::Statement::sql(format!(
            "INSERT INTO {db}.native_state (tic, p_message) VALUES (100, {held})"
        ))])
        .await
        .unwrap();
    let read = |tic: u32| {
        let sql = format!(
            "SELECT hu_message, hu_message_on, hu_message_counter, p_message \
             FROM {db}.native_state WHERE tic = {tic}"
        );
        async move { fixture.scalar::<Message>(&sql).await }
    };

    fixture
        .execute(&[sim::tick::demo_statement(db, 101, 101)])
        .await
        .unwrap();
    let taken = read(101).await;
    assert_eq!(taken.hu_message, held, "the widget shows what was held");
    assert_eq!(taken.hu_message_on, 1);
    assert_eq!(taken.hu_message_counter, MSGTIMEOUT);
    assert_eq!(taken.p_message, 0, "the player's hand is emptied");

    // The counter runs down, and the widget goes off when it hits zero.
    fixture
        .execute(&[sim::tick::demo_statement(db, 102, 101 + MSGTIMEOUT as u32)])
        .await
        .unwrap();
    let running = read(102).await;
    assert_eq!(running.hu_message_counter, MSGTIMEOUT - 1);
    assert_eq!(running.hu_message_on, 1);
    let over = read(101 + MSGTIMEOUT as u32).await;
    assert_eq!(over.hu_message_counter, 0);
    assert_eq!(
        over.hu_message_on, 0,
        "the message goes off when it times out"
    );
    assert_eq!(over.hu_message, held, "the widget keeps the line it drew");
}

#[derive(Row, Deserialize)]
struct Tic {
    tic: u32,
    leveltime: i32,
    rndindex: u8,
    st_clock: i32,
    st_randomnumber: i32,
    st_faceindex: i32,
    st_facecount: i32,
    st_priority: i32,
    st_lastattackdown: i32,
    st_oldhealth: i32,
    st_lastcalc: i32,
    st_calc_oldhealth: i32,
    p_attackdown: u8,
    menu_skullanim: i32,
    menu_whichskull: i32,
    p_cmd_forwardmove: i8,
    p_cmd_sidemove: i8,
    p_cmd_angleturn: i16,
    p_cmd_buttons: u8,
    demo_end: u8,
    texturetranslation: Vec<i32>,
    flattranslation: Vec<i32>,
    side_textureoffset: Vec<i32>,
}

/// `leveltime`, `st_clock` and the two random indices.
///
/// `M_Random` is drawn once a tic by `ST_Ticker` and 320 times by
/// `wipe_initMelt`, which runs between the tic the first frame follows and
/// that frame.
fn the_clocks_run_as_the_engine_runs_them(rows: &[Tic]) {
    let rnd = tables::table("rndtable").unwrap().ints("value").unwrap();
    let mut rndindex: u32 = 0;
    for row in rows.iter().skip(1) {
        let tic = row.tic;
        assert_eq!(row.leveltime, tic as i32, "leveltime at tic {tic}");
        assert_eq!(row.st_clock, tic as i32, "st_clock at tic {tic}");
        rndindex = (rndindex + 1) & 0xff;
        assert_eq!(
            row.st_randomnumber, rnd[rndindex as usize] as i32,
            "st_randomnumber at tic {tic}"
        );
        if tic == MELT_TIC {
            rndindex = (rndindex + MELT_DRAWS) & 0xff;
        }
        assert_eq!(u32::from(row.rndindex), rndindex, "rndindex at tic {tic}");
    }

    // M_Ticker, from a skull counter the menu left at 10.
    let (mut counter, mut skull) = (10, 0);
    for row in rows.iter().skip(1) {
        counter -= 1;
        if counter <= 0 {
            skull ^= 1;
            counter = 8;
        }
        assert_eq!(row.menu_skullanim, counter, "skullanim at tic {}", row.tic);
        assert_eq!(row.menu_whichskull, skull, "whichskull at tic {}", row.tic);
    }
}

/// The tic command, against the demo lump read a second time.
fn the_commands_are_the_demo_lump(rows: &[Tic], wad: &Wad<'_>) {
    let demo = wad
        .find(support::DEMO)
        .expect("the WAD carries DEMO3")
        .bytes;
    // `G_DoPlayDemo` reads a header of the version byte, six game settings
    // and one byte per player, then four bytes per tic command.
    let commands = &demo[13..];
    for row in rows.iter().skip(1) {
        let at = (row.tic as usize - 1) * 4;
        assert_eq!(row.demo_end, 0, "the demo ends inside the run");
        assert_eq!(
            row.p_cmd_forwardmove, commands[at] as i8,
            "forwardmove at tic {}",
            row.tic
        );
        assert_eq!(row.p_cmd_sidemove, commands[at + 1] as i8);
        assert_eq!(
            row.p_cmd_angleturn,
            i16::from(commands[at + 2] as i8) << 8,
            "angleturn at tic {}",
            row.tic
        );
        assert_eq!(row.p_cmd_buttons, commands[at + 3]);
    }
}

/// `ST_updateFaceWidget` and `ST_calcPainOffset`, against a reader written
/// from `st_stuff.c`.
///
/// Nothing in this run hurts the player or gives them a weapon, so the
/// ladder settles on the rapid-fire rung and the straight face, which are
/// the two the run exercises. `A_WeaponReady` clears the player's
/// `attackdown` once the weapon is up, and the rung follows it.
fn the_face_follows_the_status_bar_s_ladder(rows: &[Tic]) {
    let mut face = ticker::Face::default();
    for row in rows.iter().skip(1) {
        face.update(row.st_randomnumber, row.p_attackdown != 0);
        assert_eq!(
            row.st_faceindex, face.faceindex,
            "faceindex at tic {}",
            row.tic
        );
        assert_eq!(
            row.st_facecount, face.facecount,
            "facecount at tic {}",
            row.tic
        );
        assert_eq!(
            row.st_priority, face.priority,
            "priority at tic {}",
            row.tic
        );
        assert_eq!(
            row.st_lastattackdown, face.lastattackdown,
            "lastattackdown at tic {}",
            row.tic
        );
        assert_eq!(
            row.st_lastcalc, face.lastcalc,
            "lastcalc at tic {}",
            row.tic
        );
        assert_eq!(row.st_calc_oldhealth, face.calc_oldhealth);
        assert_eq!(row.st_oldhealth, ticker::PLAYER_HEALTH);
    }
}

#[derive(Row, Deserialize)]
struct Side {
    id: i32,
    textureoffset: i32,
}

/// Every side's texture offset as the map lists it, and the first side of
/// every line `P_SpawnSpecials` put on the scrolling list.
async fn sides(fixture: &Fixture) -> (Vec<i32>, Vec<i32>) {
    let db = &fixture.database;
    let all: Vec<Side> = fixture
        .rows(&format!(
            "SELECT toInt32(id) AS id, textureoffset FROM {db}.lv_sides ORDER BY id"
        ))
        .await;
    let scrollers: Vec<Side> = fixture
        .rows(&format!(
            "SELECT side0 AS id, toInt32(0) AS textureoffset FROM {db}.lv_lines \
             WHERE special = {SCROLL_LEFT} ORDER BY id"
        ))
        .await;
    assert!(!scrollers.is_empty(), "E1M7 has scrolling walls");
    (
        all.into_iter().map(|side| side.textureoffset).collect(),
        scrollers.into_iter().map(|side| side.id).collect(),
    )
}

/// `P_UpdateSpecials` moves a scrolling wall one unit a tic away from the
/// offset the map gave it, and leaves every other side alone.
fn the_scrolling_walls_move_one_unit_a_tic(rows: &[Tic], sides: &(Vec<i32>, Vec<i32>)) {
    let (offsets, scrollers) = sides;
    for row in rows {
        assert_eq!(row.side_textureoffset.len(), offsets.len());
        for (at, offset) in row.side_textureoffset.iter().enumerate() {
            let side = at as i32;
            let moved = if scrollers.contains(&side) {
                row.tic as i32 * FRACUNIT
            } else {
                0
            };
            assert_eq!(
                *offset,
                offsets[at] + moved,
                "side {side} at tic {}",
                row.tic
            );
        }
    }
}

#[derive(Row, Deserialize)]
struct Picture {
    name: String,
}

/// Every texture name and every flat name, in picture-number order.
async fn picture_names(fixture: &Fixture) -> (Vec<String>, Vec<String>) {
    let names = |table: &str| {
        let sql = format!("SELECT name FROM {}.{table} ORDER BY id", fixture.database);
        async move { fixture.rows::<Picture>(&sql).await }
    };
    let textures: Vec<String> = names("tex_textures")
        .await
        .into_iter()
        .map(|row| row.name)
        .collect();
    let flats: Vec<String> = names("flats")
        .await
        .into_iter()
        .map(|row| row.name)
        .collect();
    (textures, flats)
}

/// `P_InitPicAnims` and the animation half of `P_UpdateSpecials`, against a
/// reader written from `p_spec.c`.
fn the_animated_pictures_cycle(rows: &[Tic], names: &(Vec<String>, Vec<String>)) {
    let (textures, flats) = names;
    let anims = ticker::anims(textures, flats);
    assert!(!anims.is_empty(), "E1M7's WAD carries animated pictures");
    for row in rows.iter().skip(1) {
        // `P_UpdateSpecials` runs before `leveltime` is bumped.
        let leveltime = row.leveltime - 1;
        let want = |istexture: bool, count: usize| {
            ticker::translation(&anims, istexture, count, leveltime)
        };
        assert_eq!(
            row.texturetranslation,
            want(true, textures.len()),
            "texturetranslation at tic {}",
            row.tic
        );
        assert_eq!(
            row.flattranslation,
            want(false, flats.len()),
            "flattranslation at tic {}",
            row.tic
        );
    }
}
