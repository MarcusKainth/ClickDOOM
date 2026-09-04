//! The tic transform against the reference emulator, on a real server.
//!
//! Every field the committed probe fixture covers has to agree with the
//! engine, apart from a named set the simulation does not compute. Then
//! the player walks into the wall demo3
//! puts in front of it, and the position and momentum `P_SlideMove` leaves
//! are checked against the engine's own. Then the weapon sprite walks up
//! the screen, bobs, and is swapped for the shotgun the player picks up,
//! against the engine's own positions. Then the things on the list cycle
//! their states, `A_Look` takes the player as the first monster's target on
//! the tic the engine does, `A_Chase` walks it towards the player from the
//! tic after, and its missile check draws on the tic the engine's own
//! random-call log records. Then the player fires: the mobj enters its
//! attack frames, the weapon sprite the shotgun's, and the sectors the
//! noise reaches hold the player at the count the engine leaves them on.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use std::path::Path;

use clickdoom_native::sql::{parity, probe, sim};
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;

/// The engine's state at the frame commits the fixture keeps, which are
/// the last frame of gametics 2, 3, 4 and 963.
fn fixture_tsv() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../refemu/probe/fixtures/demo3-frames.9a6a47d01119.tsv");
    std::fs::read_to_string(path).expect("the probe fixture is committed")
}

/// Fields the simulation does not compute, with the group they sit in.
///
/// Every field the fixture reaches agrees with the engine, so this is
/// empty. A field that starts differing fails the test, and so does one
/// named here that agrees, so the list cannot outlive what it excuses.
const OPEN: [&str; 0] = [];

/// How far the walk runs. Gametic 32 is where demo3 first puts a wall in
/// the way, the tics after it are the slide along that wall, the door the
/// press at 73 opens has reached the top and left the list by 120, and 205
/// is the last tic before the engine's own monsters change where the
/// player ends up.
const WALK_TICS: u32 = 205;

/// `p_local.h`: the use key's bit in a tic command.
const BT_USE: u8 = 2;

/// `gametic, thinkers, sec_ceilingheight[63], sec_specialdata[63],
/// line_special[951]` around the door demo3 opens, read out of the
/// reference emulator's trace. The press at 73 makes the thinker and
/// spends the line's special, the ceiling rises a step a tic, and the
/// thinker comes off the list on the tic it reaches the top.
const DOOR: [(u32, usize, i32, u32, i16); 6] = [
    (72, 16, 0, 0, 31),
    (73, 17, 131072, 17, 0),
    (80, 17, 1048576, 17, 0),
    (106, 17, 4456448, 17, 0),
    (107, 16, 4456448, 0, 0),
    (120, 16, 4456448, 0, 0),
];

/// A tic the use key goes down on and the press reaches nothing special.
/// The engine plays a sound and moves on, and so does the simulation.
const USE_INTO_NOTHING: u32 = 42;

/// The tic demo3's first monster acts on the player: `A_Look` takes the
/// player as its target and the see state it enters carries `A_Chase`,
/// which runs on the same tic.
const FIRST_CHASE: u32 = 77;

/// The first tic two monsters stand close enough that one's move changes
/// what the other is told. The engine runs them one after the other and
/// this runs them together, so the tic says it could not be produced.
const FIRST_CROWDED: u32 = 82;

/// The tic the reference run's random-call log records
/// `P_CheckMissileRange`'s draw for the distance on. The row it produces
/// is gametic 125.
const MISSILE_DRAW: u32 = 125;

/// The tic `A_WeaponReady` reaches `P_FireWeapon`: the player's mobj goes
/// into its attack frames, the weapon sprite into the shotgun's, and the
/// noise goes out.
const FIRST_SHOT: u32 = 140;

/// The tic the shotgun's own frames reach `A_FireShotgun`, which this
/// does not run.
const FIRST_SHOT_FRAME: u32 = 143;

/// One tic of the shot, read out of the reference emulator's demo3 trace:
/// the weapon sprite, the player's mobj and its wait, whether the attack
/// button is marked down, and how many sectors the alert leaves at each
/// count.
struct Shot {
    tic: u32,
    psp_state: i32,
    pl_state: i32,
    pl_tics: i32,
    attackdown: u8,
    near: u64,
    far: u64,
    unreached: u64,
}

/// Gametic 139 is the last tic the weapon is ready and nothing has heard
/// anything, 140 the shot, and 142 the alert still standing while the
/// shotgun's own frames run.
const SHOT: [Shot; 3] = [
    Shot {
        tic: 139,
        psp_state: 18,
        pl_state: 152,
        pl_tics: 1,
        attackdown: 0,
        near: 0,
        far: 0,
        unreached: 170,
    },
    Shot {
        tic: FIRST_SHOT,
        psp_state: 21,
        pl_state: 154,
        pl_tics: 11,
        attackdown: 1,
        near: 53,
        far: 5,
        unreached: 112,
    },
    Shot {
        tic: 142,
        psp_state: 21,
        pl_state: 154,
        pl_tics: 9,
        attackdown: 1,
        near: 53,
        far: 5,
        unreached: 112,
    },
];

/// `gametic, prndindex` read out of the reference emulator's demo3 trace.
///
/// The index holds still on a tic that draws nothing and moves by one for
/// each draw. Gametic 77 is the first tic a monster acts, 125 the first
/// tic the missile check draws, and 140 to 142 are the monsters the shot's
/// own noise wakes.
const RANDOM: [(u32, u8); 12] = [
    (2, 209),
    (40, 226),
    (61, 233),
    (76, 241),
    (77, 244),
    (110, 83),
    (124, 121),
    (MISSILE_DRAW, 126),
    (139, 170),
    (FIRST_SHOT, 178),
    (141, 189),
    (142, 194),
];

/// `gametic, m_movedir, m_movecount, m_reactiontime, m_angle, m_x, m_y`
/// for that monster, read out of the reference emulator's demo3 trace.
/// Gametic 76 is the last tic it stands still, 77 the tic it wakes, turns
/// a step towards the way it is about to walk and takes its first, 80 the
/// next tic its state cycle reaches `A_Chase`, 125 the tic its missile
/// check draws, and 139 the last tic before the player shoots.
const CHASE: [(u32, i32, i32, i32, u32, i32, i32); 5] = [
    (76, 0, 0, 8, 536870912, 0, -12582912),
    (FIRST_CHASE, 1, 15, 7, 0, 376000, -12206912),
    (80, 1, 14, 6, 536870912, 752000, -11830912),
    (MISSILE_DRAW, 1, 11, 0, 536870912, 6392000, -6190912),
    (139, 1, 7, 0, 536870912, 7896000, -4686912),
];

/// `gametic, m_state[118], m_frame[118], m_health[118]` for the monster a
/// pellet of the first shot reaches, read out of the reference emulator's
/// demo3 trace. Gametic 142 is the last tic before the shot lands and 143
/// the tic it does: `P_DamageMobj` takes ten off the monster and puts it
/// in its pain state, and `P_SetMobjState` writes that state's frame with
/// the state.
const HURT: [(u32, i32, i32, i32); 2] = [(142, 449, 2, 60), (FIRST_SHOT_FRAME, 455, 7, 50)];

/// `gametic, slot, m_x, m_y, m_momx, m_momy` for the two monsters a pellet
/// thrusts, read out of the reference emulator's demo3 trace. Each stands
/// still at gametic 142 and spends what `P_DamageMobj` gave it at 143:
/// `P_XYMovement` moves it by the whole of that momentum and friction
/// takes the rest.
const THRUST: [(u32, usize, i32, i32, i32, i32); 4] = [
    (142, 118, 8272000, -4310912, 0, 0),
    (142, 258, 13992912, 4297488, 0, 0),
    (FIRST_SHOT_FRAME, 118, 8245976, -4388589, -23585, -70395),
    (FIRST_SHOT_FRAME, 258, 14040150, 4184052, 42809, -102802),
];

/// `gametic, m_state[118], m_target[118]` around that monster, and the
/// state cycle of the thing in slot 25, read out of the reference
/// emulator's demo3 trace. Slot 25 alternates two stand frames from the
/// first tic; slot 118 stands until `A_Look` sees the player.
const THINGS: [(u32, i32, i32, u32, i32); 5] = [
    (2, 208, 1, 0, 840),
    (10, 208, 1, 0, 840),
    (40, 207, 0, 0, 840),
    (76, 207, 0, 0, 443),
    (FIRST_CHASE, 207, 0, 1, 444),
];

/// `gametic, psp_state, psp_sx, psp_sy, p_readyweapon, p_pendingweapon,
/// p_attackdown` for the weapon sprite, read out of the reference
/// emulator's demo3 trace. Gametic 2 and 14 are `A_Raise` walking the
/// pistol up the screen, 15 the first tic `A_WeaponReady` bobs it, 47 the
/// shotgun pickup asking for a weapon, 48 `A_Lower` starting to put the
/// pistol away, 63 `P_BringUpWeapon` bringing the shotgun up, and 139 the
/// last tic before the first shot.
const WEAPON: [(u32, i32, i32, i32, i32, i32, u8); 7] = [
    (2, 12, 0, 7208960, 1, 10, 1),
    (14, 12, 0, 2490368, 1, 10, 1),
    (15, 10, 98903, 2265249, 1, 10, 0),
    (47, 10, 23335, 2309743, 1, 2, 0),
    (48, 11, 23335, 2702959, 1, 2, 0),
    (63, 20, 23335, 7995392, 2, 10, 0),
    (139, 18, 602121, 2900895, 2, 10, 0),
];

/// `gametic, m_x, m_y, m_momx, m_momy` for the player, read out of the
/// reference emulator's demo3 trace. Gametic 31 is the last free move, 32
/// is the blocked one, and the rest are the slide.
const WALK: [(u32, i32, i32, i32, i32); 8] = [
    (2, 6225766, 34419194, -28303, -79195),
    (31, 4639872, 25605989, 78408, -506265),
    (32, 4756160, 25182290, 17337, 0),
    (33, 4847534, 25166570, 19647, 0),
    (34, 4901210, 25166337, 47338, 0),
    (40, 5126423, 25166337, 26222, 0),
    (150, 12261441, 7984570, -23457, 134828),
    (205, 10419829, 14857119, -31105, 38150),
];

#[derive(Row, Deserialize)]
struct Divergence {
    field: String,
    kind: String,
    /// How many of the compared tics differ in this field.
    tics: u64,
    first_tic: u32,
    slot: u32,
    ours: String,
    theirs: String,
}

#[derive(Row, Deserialize)]
struct Walked {
    tic: u32,
    x: i32,
    y: i32,
    momx: i32,
    momy: i32,
    unresolved: u8,
    buttons: u8,
    thinkers: u64,
    ceiling: i32,
    specialdata: u32,
    special: i16,
    state25: i32,
    frame25: i32,
    state118: i32,
    target118: u32,
    lastlook34: i32,
    pl_state: i32,
    pl_tics: i32,
    heard_near: u64,
    heard_far: u64,
    heard_none: u64,
    heard_of: u64,
    psp_state: Vec<i32>,
    psp_sx: Vec<i32>,
    psp_sy: Vec<i32>,
    readyweapon: i32,
    pendingweapon: i32,
    attackdown: u8,
    prndindex: u8,
    movedir118: i32,
    movecount118: i32,
    reactiontime118: i32,
    angle118: u32,
    x118: i32,
    y118: i32,
    frame118: i32,
    health118: i32,
    thrust_x: Vec<i32>,
    thrust_y: Vec<i32>,
    thrust_momx: Vec<i32>,
    thrust_momy: Vec<i32>,
}

async fn walked(fixture: &Fixture, db: &str) -> Vec<Walked> {
    fixture
        .rows(&format!(
            "SELECT tic, m_x[p_mo] AS x, m_y[p_mo] AS y, \
             m_momx[p_mo] AS momx, m_momy[p_mo] AS momy, \
             unresolved, toUInt8(p_cmd_buttons) AS buttons, \
             toUInt64(length(s_kind)) AS thinkers, sec_ceilingheight[63] AS ceiling, \
             sec_specialdata[63] AS specialdata, line_special[951] AS special, \
             m_state[25] AS state25, m_frame[25] AS frame25, \
             m_state[118] AS state118, m_target[118] AS target118, \
             m_lastlook[34] AS lastlook34, \
             m_state[p_mo] AS pl_state, m_tics[p_mo] AS pl_tics, \
             toUInt64(countEqual(sec_soundtraversed, 1)) AS heard_near, \
             toUInt64(countEqual(sec_soundtraversed, 2)) AS heard_far, \
             toUInt64(countEqual(sec_soundtraversed, 0)) AS heard_none, \
             toUInt64(countEqual(sec_soundtarget, p_mo)) AS heard_of, \
             psp_state, psp_sx, psp_sy, \
             p_readyweapon AS readyweapon, p_pendingweapon AS pendingweapon, \
             p_attackdown AS attackdown, prndindex, \
             m_movedir[118] AS movedir118, m_movecount[118] AS movecount118, \
             m_reactiontime[118] AS reactiontime118, m_angle[118] AS angle118, \
             m_x[118] AS x118, m_y[118] AS y118, \
             m_frame[118] AS frame118, m_health[118] AS health118, \
             arrayMap(k -> m_x[k], [118, 258]) AS thrust_x, \
             arrayMap(k -> m_y[k], [118, 258]) AS thrust_y, \
             arrayMap(k -> m_momx[k], [118, 258]) AS thrust_momx, \
             arrayMap(k -> m_momy[k], [118, 258]) AS thrust_momy \
             FROM {db}.native_state ORDER BY tic"
        ))
        .await
}

#[tokio::test]
async fn the_tic_matches_the_engine_where_the_fixture_reaches() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_parity").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.push(probe::schema_statement(&db));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, WALK_TICS));
    plan.push(probe::insert(&db, &fixture_tsv()).unwrap());
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let summary: Vec<Divergence> = fixture.rows(&parity::field_summary(&db)).await;
    let walk = walked(&fixture, &db).await;
    fixture.finish().await;

    let differ: Vec<&str> = summary.iter().map(|d| d.field.as_str()).collect();
    let unexpected: Vec<&Divergence> = summary
        .iter()
        .filter(|d| !OPEN.contains(&d.field.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "fields that no longer match the engine: {}",
        unexpected
            .iter()
            .map(|d| format!(
                "{} ({}) differs on {} tics, first at {} slot {}, ours {} theirs {}",
                d.field, d.kind, d.tics, d.first_tic, d.slot, d.ours, d.theirs
            ))
            .collect::<Vec<_>>()
            .join("; ")
    );
    assert_eq!(
        differ.len(),
        OPEN.len(),
        "a field named open now agrees, so the list excuses more than it \
         has to: {differ:?}"
    );

    // The setup row the level leaves behind is tic 0, so the run holds
    // one row more than the tics it was asked for.
    assert_eq!(walk.len(), WALK_TICS as usize + 1, "every tic ran");

    let at = |tic: u32| {
        walk.iter()
            .find(|row| row.tic == tic)
            .unwrap_or_else(|| panic!("gametic {tic} ran"))
    };
    // The run reaches past the door, and every tic up to the first shot
    // completes.
    for row in walk.iter().filter(|row| row.tic < FIRST_CROWDED) {
        assert_eq!(
            row.unresolved, 0,
            "gametic {} was not carried through",
            row.tic
        );
    }
    assert_eq!(
        at(FIRST_CROWDED).unresolved,
        1,
        "the tic two monsters stand close enough says it could not be produced"
    );
    let pressed = at(USE_INTO_NOTHING);
    assert_eq!(pressed.buttons & BT_USE, BT_USE, "the use key is down");
    assert_eq!(
        pressed.unresolved, 0,
        "a press that reaches no special line finishes like any other tic"
    );
    for (tic, thinkers, ceiling, specialdata, special) in DOOR {
        let row = at(tic);
        assert_eq!(
            (row.thinkers, row.ceiling, row.specialdata, row.special),
            (thinkers as u64, ceiling, specialdata, special),
            "the door at gametic {tic}"
        );
    }

    for (tic, x, y, momx, momy) in WALK {
        let at = at(tic);
        assert_eq!(
            (at.x, at.y, at.momx, at.momy),
            (x, y, momx, momy),
            "gametic {tic}"
        );
    }

    for (tic, state, sx, sy, ready, pending, attackdown) in WEAPON {
        let row = at(tic);
        assert_eq!(
            (
                row.psp_state[0],
                row.psp_sx[0],
                row.psp_sy[0],
                row.readyweapon,
                row.pendingweapon,
                row.attackdown
            ),
            (state, sx, sy, ready, pending, attackdown),
            "the weapon sprite at gametic {tic}"
        );
    }
    for (tic, state, frame, target, awake) in THINGS {
        let row = at(tic);
        assert_eq!(
            (row.state25, row.frame25, row.target118, row.state118),
            (state, frame, target, awake),
            "the things at gametic {tic}"
        );
    }
    for shot in SHOT {
        let row = at(shot.tic);
        assert_eq!(
            (
                row.psp_state[0],
                row.pl_state,
                row.pl_tics,
                row.attackdown,
                row.heard_near,
                row.heard_far,
                row.heard_none
            ),
            (
                shot.psp_state,
                shot.pl_state,
                shot.pl_tics,
                shot.attackdown,
                shot.near,
                shot.far,
                shot.unreached
            ),
            "the shot at gametic {}",
            shot.tic
        );
        assert_eq!(
            row.heard_of,
            shot.near + shot.far,
            "every sector the alert reached holds the player at gametic {}",
            shot.tic
        );
    }
    assert_eq!(
        at(FIRST_SHOT_FRAME).unresolved,
        1,
        "the tic the shotgun's frames reach A_FireShotgun says it could not be produced"
    );

    for (tic, prndindex) in RANDOM {
        assert_eq!(
            at(tic).prndindex,
            prndindex,
            "the random index at gametic {tic}"
        );
    }
    for (tic, movedir, movecount, reactiontime, angle, x, y) in CHASE {
        let row = at(tic);
        assert_eq!(
            (
                row.movedir118,
                row.movecount118,
                row.reactiontime118,
                row.angle118,
                row.x118,
                row.y118
            ),
            (movedir, movecount, reactiontime, angle, x, y),
            "the chasing monster at gametic {tic}"
        );
    }
    for (tic, slot, x, y, momx, momy) in THRUST {
        let row = at(tic);
        let place = if slot == 118 { 0 } else { 1 };
        assert_eq!(
            (
                row.thrust_x[place],
                row.thrust_y[place],
                row.thrust_momx[place],
                row.thrust_momy[place]
            ),
            (x, y, momx, momy),
            "the monster a pellet thrusts, slot {slot} at gametic {tic}"
        );
    }
    for (tic, state, frame, health) in HURT {
        let row = at(tic);
        assert_eq!(
            (row.state118, row.frame118, row.health118),
            (state, frame, health),
            "the monster a pellet reaches at gametic {tic}"
        );
    }
    // `P_LookForPlayers` walks `lastlook` round to the one player in the
    // game and stops there, whatever it decides.
    for row in walk.iter().filter(|row| row.tic >= 2) {
        assert_eq!(row.lastlook34, 0, "lastlook at gametic {}", row.tic);
    }

    // `P_MovePsprites` ends by putting the flash sprite where the weapon
    // sprite is, whatever state either is in. The level's own row is
    // before the first `P_PlayerThink`, so it is not one of them.
    for row in walk.iter().filter(|row| row.tic > 0) {
        assert_eq!(
            (row.psp_sx[0], row.psp_sy[0]),
            (row.psp_sx[1], row.psp_sy[1]),
            "the flash sprite follows the weapon at gametic {}",
            row.tic
        );
    }
}
