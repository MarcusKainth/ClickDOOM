//! The player's tic, from `p_user.c`, and the mobj behind it.
//!
//! `P_PlayerThink` runs before the thinkers do, so it reads the position
//! the tic before it left and sets the momentum the mobj then moves with.
//! `P_CalcHeight` sits between the two, which is why the bob a frame draws
//! comes from the momentum before friction rather than after it.

use crate::sql::{Statement, bind, fixed};

use super::map::World;
use super::mobj::{self, Mover, Pickups};
use super::{State, inter, maputl, noise, pspr};

/// `p_local.h`
const VIEWHEIGHT: i64 = 41 << 16;
const MAXBOB: i64 = 0x10_0000;
/// `tables.h`
const ANG90: i64 = 0x4000_0000;
const ANGLETOFINESHIFT: u32 = 19;
/// `d_player.h`
const CF_NOCLIP: i64 = 1;
/// `p_mobj.h`
const MF_NOCLIP: i64 = 0x1000;
/// `d_event.h`
const BT_SPECIAL: i64 = 128;
const BT_CHANGE: i64 = 4;
const BT_WEAPONMASK: i64 = 8 + 16 + 32;
const BT_WEAPONSHIFT: u32 = 3;
const BT_USE: i64 = 2;
/// `doomdef.h`
const WP_PLASMA: i64 = 5;
const WP_BFG: i64 = 6;
const WP_FIST: i64 = 0;
const WP_CHAINSAW: i64 = 7;
/// `d_player.h`: the powers a tic counts down, one-based.
const PW_INVULNERABILITY: usize = 1;
const PW_STRENGTH: usize = 2;
const PW_INVISIBILITY: usize = 3;
const PW_IRONFEET: usize = 4;
const PW_INFRARED: usize = 6;
/// `p_user.c`: the map the invulnerability sphere inverts the screen with.
const INVERSECOLORMAP: i64 = 32;

/// The constants the player's tic reads.
pub fn constants(db: &str) -> Vec<(String, String)> {
    vec![
        (
            "s_play".to_owned(),
            format!("assumeNotNull((SELECT spawnstate FROM {db}.mobjinfo WHERE id = 0))"),
        ),
        (
            "s_play_run1".to_owned(),
            format!("assumeNotNull((SELECT seestate FROM {db}.mobjinfo WHERE id = 0))"),
        ),
        (
            "s_play_atk1".to_owned(),
            format!("assumeNotNull((SELECT missilestate FROM {db}.mobjinfo WHERE id = 0))"),
        ),
        // The second attack frame is not named by any table. It is the one
        // state that runs back into the first, and `guards` stops the load
        // unless exactly one does.
        (
            "s_play_atk2".to_owned(),
            format!(
                "assumeNotNull((SELECT id FROM {db}.states WHERE nextstate = \
                 (SELECT missilestate FROM {db}.mobjinfo WHERE id = 0) LIMIT 1))"
            ),
        ),
        (
            "skill".to_owned(),
            format!("toInt32(assumeNotNull((SELECT h.skill FROM {db}.demo_header AS h)))"),
        ),
    ]
}

/// What stops the load: the engine's second player attack frame not being
/// the one state that runs back into the first.
///
/// `A_WeaponReady` compares the player's mobj against both frames, and no
/// table names the second.
pub fn guards(db: &str) -> Vec<Statement> {
    vec![Statement::sql(format!(
        "SELECT throwIf(count() != 1, 'A_WeaponReady: the second attack frame is not one state')\n\
         FROM {db}.states\n\
         WHERE nextstate = (SELECT missilestate FROM {db}.mobjinfo WHERE id = 0)"
    ))]
}

/// `P_PlayerThink` and the mobj thinker that follows it.
pub fn think(state: &State) -> Vec<(String, String)> {
    let mut bindings = read(state);
    bindings.extend(move_player(state));
    bindings.extend(calc_height(state));
    bindings.extend(special_sector(state));
    bindings.extend(weapon_and_use(state));
    bindings.extend(pspr::move_psprites(
        state,
        "now_p_bob",
        "pl_buttons",
        "pl_pendingweapon",
    ));
    bindings.extend(fire_weapon(state));
    bindings.extend(powers(state));
    bindings.extend(mobj_thinker(state));
    bindings.extend(writeback(state));
    bindings
}

/// The player's mobj, read out of the arrays once.
fn read(state: &State) -> Vec<(String, String)> {
    let at = state.get("p_mo");
    let field = |name: &str| {
        (
            format!("pl_{name}"),
            format!("{}[{at}]", state.get(&format!("m_{name}"))),
        )
    };
    let mut bindings = vec![("pl_slot".to_owned(), at.clone())];
    for name in [
        "x",
        "y",
        "z",
        "angle",
        "momx",
        "momy",
        "momz",
        "floorz",
        "ceilingz",
        "radius",
        "height",
        "flags",
        "state",
        "tics",
        "health",
        "subsector",
    ] {
        bindings.push(field(name));
    }
    // `P_PlayerThink` puts the no-clip cheat on the mobj before anything
    // else reads the flags.
    bindings.push((
        "pl_flags_noclip".to_owned(),
        format!(
            "toInt32(if(bitAnd({}, {CF_NOCLIP}) != 0, bitOr(pl_flags, {MF_NOCLIP}), \
             bitAnd(pl_flags, {})))",
            state.get("p_cheats"),
            !MF_NOCLIP
        ),
    ));
    bindings
}

/// `P_MovePlayer` and `P_Thrust`: the command turns the mobj and pushes it.
fn move_player(state: &State) -> Vec<(String, String)> {
    let angle = format!(
        "toUInt32(bitAnd(toInt64(pl_angle) + \
         bitShiftLeft(toInt64({}), 16), 4294967295))",
        state.get("p_cmd_angleturn")
    );
    let thrust = |move_: &str, turn: i64| {
        let fine = format!(
            "toUInt32(bitShiftRight(bitAnd(toInt64(pl_new_angle) + {turn}, 4294967295), \
             {ANGLETOFINESHIFT}))"
        );
        (
            fixed::fixed_mul(&format!("({move_}) * 2048"), &maputl::finecosine(&fine)),
            fixed::fixed_mul(&format!("({move_}) * 2048"), &maputl::finesine(&fine)),
        )
    };
    let forward = state.get("p_cmd_forwardmove");
    let side = state.get("p_cmd_sidemove");
    let (fx, fy) = thrust(&format!("toInt64({forward})"), 0);
    let (sx, sy) = thrust(&format!("toInt64({side})"), -ANG90);
    vec![
        ("pl_new_angle".to_owned(), angle),
        (
            "pl_onground".to_owned(),
            "toUInt8(toInt64(pl_z) <= toInt64(pl_floorz))".to_owned(),
        ),
        (
            "pl_pushx".to_owned(),
            format!(
                "toInt32(toInt64(pl_momx) + \
                 if({forward} != 0 AND pl_onground = 1, toInt64({fx}), 0) + \
                 if({side} != 0 AND pl_onground = 1, toInt64({sx}), 0))"
            ),
        ),
        (
            "pl_pushy".to_owned(),
            format!(
                "toInt32(toInt64(pl_momy) + \
                 if({forward} != 0 AND pl_onground = 1, toInt64({fy}), 0) + \
                 if({side} != 0 AND pl_onground = 1, toInt64({sy}), 0))"
            ),
        ),
        (
            "pl_runs".to_owned(),
            format!("toUInt8(({forward} != 0 OR {side} != 0) AND pl_state = s_play)"),
        ),
        (
            "pl_state_moved".to_owned(),
            "toInt32(if(pl_runs = 1, s_play_run1, pl_state))".to_owned(),
        ),
        (
            "pl_tics_moved".to_owned(),
            "toInt32(if(pl_runs = 1, state_tics[1 + s_play_run1], pl_tics))".to_owned(),
        ),
    ]
}

/// `P_CalcHeight`: the bob comes from the momentum the push left, before
/// the mobj moves and friction takes some of it back.
fn calc_height(state: &State) -> Vec<(String, String)> {
    let bob = format!(
        "least(bitShiftRight(toInt64({}) + toInt64({}), 2), {MAXBOB})",
        fixed::fixed_mul("pl_pushx", "pl_pushx"),
        fixed::fixed_mul("pl_pushy", "pl_pushy"),
    );
    let viewheight = state.get("p_viewheight");
    let delta = state.get("p_deltaviewheight");
    let angle = format!(
        "toUInt32(bitAnd(intDiv(8192, 20) * toInt64({}), 8191))",
        state.get("leveltime")
    );
    let raised = "least(toInt64(pl_view_stepped) + toInt64(pl_delta_stepped), 100000000)";
    vec![
        ("now_p_bob".to_owned(), format!("toInt32({bob})")),
        (
            "pl_bobamt".to_owned(),
            fixed::fixed_mul(
                "toInt32(bitShiftRight(toInt64(now_p_bob), 1))",
                &maputl::finesine(&angle),
            ),
        ),
        // The view rises back to its resting height, and the rise slows.
        (
            "pl_view_stepped".to_owned(),
            format!("toInt32({viewheight})"),
        ),
        ("pl_delta_stepped".to_owned(), format!("toInt32({delta})")),
        (
            "pl_view_raised".to_owned(),
            format!("toInt64(if(pl_onground = 1, {raised}, toInt64(pl_view_stepped)))"),
        ),
        (
            "now_p_viewheight".to_owned(),
            format!(
                "toInt32(multiIf(pl_onground = 0, pl_view_stepped, \
                 pl_view_raised > {VIEWHEIGHT}, {VIEWHEIGHT}, \
                 pl_view_raised < {}, {}, pl_view_raised))",
                VIEWHEIGHT / 2,
                VIEWHEIGHT / 2
            ),
        ),
        (
            "pl_delta_clamped".to_owned(),
            format!(
                "toInt64(multiIf(pl_onground = 0, toInt64(pl_delta_stepped), \
                 pl_view_raised > {VIEWHEIGHT}, 0, \
                 pl_view_raised < {} AND toInt64(pl_delta_stepped) <= 0, 1, \
                 toInt64(pl_delta_stepped)))",
                VIEWHEIGHT / 2
            ),
        ),
        (
            "now_p_deltaviewheight".to_owned(),
            "toInt32(if(pl_onground = 1 AND pl_delta_clamped != 0, \
             if(pl_delta_clamped + 16384 = 0, 1, pl_delta_clamped + 16384), \
             pl_delta_clamped))"
                .to_owned(),
        ),
        (
            "now_p_viewz".to_owned(),
            format!(
                "toInt32(least(if(pl_onground = 1, \
                 toInt64(pl_z) + toInt64(now_p_viewheight) + toInt64(pl_bobamt), \
                 toInt64(pl_z) + toInt64(now_p_viewheight)), \
                 toInt64(pl_ceilingz) - {}))",
                4i64 << 16
            ),
        ),
    ]
}

/// `P_PlayerInSpecialSector`: only the secret this level can reach, and
/// the damaging floors it also carries.
fn special_sector(state: &State) -> Vec<(String, String)> {
    let sector = "1 + ssec_sector[1 + pl_subsector]";
    let special = format!("{}[{sector}]", state.get("sec_special"));
    vec![
        (
            "pl_on_floor".to_owned(),
            format!(
                "toUInt8(toInt64(pl_z) = toInt64({}[{sector}]))",
                state.get("sec_floorheight")
            ),
        ),
        (
            "pl_secret".to_owned(),
            format!("toUInt8(pl_on_floor = 1 AND {special} = 9)"),
        ),
        (
            "pl_hurts".to_owned(),
            format!(
                "toUInt8(pl_on_floor = 1 AND {special} IN (4, 5, 7, 11, 16) \
                 AND bitAnd(toInt64({}), 31) = 0)",
                state.get("leveltime")
            ),
        ),
        (
            "now_p_secretcount".to_owned(),
            format!(
                "toInt32({} + if(pl_secret = 1, 1, 0))",
                state.get("p_secretcount")
            ),
        ),
        (
            "now_sec_special".to_owned(),
            format!(
                "arrayMap((v, i) -> toInt16(if(pl_secret = 1 AND i = {sector}, 0, v)), \
                 {special_all}, arrayEnumerate({special_all}))",
                special_all = state.get("sec_special")
            ),
        ),
    ]
}

/// The weapon the command asks for, and whether the use key is down.
fn weapon_and_use(state: &State) -> Vec<(String, String)> {
    let buttons = format!(
        "toInt64(if(bitAnd(toInt64({b}), {BT_SPECIAL}) != 0, 0, toInt64({b})))",
        b = state.get("p_cmd_buttons")
    );
    let asked = format!("bitShiftRight(bitAnd(pl_buttons, {BT_WEAPONMASK}), {BT_WEAPONSHIFT})");
    let owned = format!("{}[1 + pl_wanted]", state.get("p_weaponowned"));
    let ready = state.get("p_readyweapon");
    vec![
        ("pl_buttons".to_owned(), buttons),
        (
            "pl_wanted".to_owned(),
            format!(
                "toInt64(if({asked} = {WP_FIST} AND {}[1 + {WP_CHAINSAW}] != 0 \
                 AND NOT ({ready} = {WP_CHAINSAW} AND {}[{PW_STRENGTH}] != 0), \
                 {WP_CHAINSAW}, {asked}))",
                state.get("p_weaponowned"),
                state.get("p_powers")
            ),
        ),
        // The pickups run after this and may ask for a weapon of their
        // own, so the command's answer is a value the tic carries rather
        // than the column, which `writeback` names from what they left.
        (
            "pl_pendingweapon".to_owned(),
            format!(
                "toInt32(if(bitAnd(pl_buttons, {BT_CHANGE}) != 0 AND {owned} != 0 \
                 AND pl_wanted != {ready} \
                 AND pl_wanted != {WP_PLASMA} AND pl_wanted != {WP_BFG}, \
                 pl_wanted, {}))",
                state.get("p_pendingweapon")
            ),
        ),
        (
            "pl_uses".to_owned(),
            format!(
                "toUInt8(bitAnd(pl_buttons, {BT_USE}) != 0 AND {} = 0)",
                state.get("p_usedown")
            ),
        ),
        (
            "now_p_usedown".to_owned(),
            format!("toUInt8(bitAnd(pl_buttons, {BT_USE}) != 0)"),
        ),
    ]
}

/// What `A_WeaponReady` and `P_FireWeapon` leave behind: the player's mobj
/// in or out of its attack frames, and the sectors the shot was heard in.
///
/// `A_WeaponReady` puts the mobj back into `S_PLAY` where it stands in
/// either attack frame, and `P_FireWeapon` puts it into the first of them,
/// so a tic that fires from an attack frame sets the state twice and the
/// wait comes from the frame it ends on.
///
/// The alert is the body of a fold over a list of one entry or none, so a
/// tic that does not fire pays for the list and not the flood.
fn fire_weapon(state: &State) -> Vec<(String, String)> {
    let target = state.get("sec_soundtarget");
    let traversed = state.get("sec_soundtraversed");
    // The sector the shot was fired in travels as the fold's own element,
    // so the flood reads a lambda parameter. An expression that reads
    // neither is evaluated once for the row whatever the fold does.
    let flood = noise::alert(
        "nz_shot",
        &state.get("sec_floorheight"),
        &state.get("sec_ceilingheight"),
    );
    // A sector the flood does not reach keeps the target and the count it
    // held, which is what `validcount` does for the walk.
    let body = bind::chain_in(
        "nza",
        &[("nz_reached".to_owned(), flood)],
        "(arrayMap((r, t) -> toUInt32(if(r != 0, pl_slot, t)), nz_reached, nz_at.1), \
         arrayMap((r, v) -> toInt32(if(r != 0, r, v)), nz_reached, nz_at.2))",
    );
    vec![
        (
            "pl_state_set".to_owned(),
            "toUInt8(psp_fired = 1 OR (psp_readied = 1 \
             AND (pl_state_moved = s_play_atk1 OR pl_state_moved = s_play_atk2)))"
                .to_owned(),
        ),
        (
            "pl_state_fired".to_owned(),
            "toInt32(multiIf(psp_fired = 1, s_play_atk1, pl_state_set = 1, s_play, \
             pl_state_moved))"
                .to_owned(),
        ),
        (
            "pl_tics_fired".to_owned(),
            "toInt32(if(pl_state_set = 1, state_tics[1 + pl_state_fired], pl_tics_moved))"
                .to_owned(),
        ),
        (
            "nz_alerted".to_owned(),
            format!(
                "arrayFold((nz_at, nz_shot) -> {body}, \
                 arraySlice([toInt32(ssec_sector[1 + pl_subsector])], 1, toUInt8(psp_fired)), \
                 (arrayMap(v -> toUInt32(v), {target}), arrayMap(v -> toInt32(v), {traversed})))"
            ),
        ),
        ("nz_soundtarget".to_owned(), "nz_alerted.1".to_owned()),
        (
            "now_sec_soundtraversed".to_owned(),
            "nz_alerted.2".to_owned(),
        ),
    ]
}

/// The counters a tic runs down, and the colormap they pick.
fn powers(state: &State) -> Vec<(String, String)> {
    let powers = state.get("p_powers");
    let down =
        |at: usize| format!("if(i = {at} AND v != 0, if({at} = {PW_STRENGTH}, v + 1, v - 1), v)");
    let counted = format!(
        "arrayMap((v, i) -> toInt32(multiIf(\
         i = {PW_STRENGTH} AND v != 0, v + 1, \
         i IN ({PW_INVULNERABILITY}, {PW_INVISIBILITY}, {PW_INFRARED}, {PW_IRONFEET}) \
         AND v != 0, v - 1, v)), {powers}, arrayEnumerate({powers}))"
    );
    let _ = down(0);
    vec![
        ("pl_powers".to_owned(), counted),
        (
            "pl_shadow_gone".to_owned(),
            format!(
                "toUInt8({powers}[{PW_INVISIBILITY}] != 0 AND pl_powers[{PW_INVISIBILITY}] = 0)"
            ),
        ),
        (
            "now_p_damagecount".to_owned(),
            format!("toInt32(greatest({} - 1, 0))", state.get("p_damagecount")),
        ),
        (
            "pl_bonuscount_down".to_owned(),
            format!("toInt32(greatest({} - 1, 0))", state.get("p_bonuscount")),
        ),
        (
            "now_p_fixedcolormap".to_owned(),
            format!(
                "toInt32(multiIf(\
                 pl_powers[{PW_INVULNERABILITY}] != 0, \
                 if(pl_powers[{PW_INVULNERABILITY}] > 128 OR \
                 bitAnd(pl_powers[{PW_INVULNERABILITY}], 8) != 0, {INVERSECOLORMAP}, 0), \
                 pl_powers[{PW_INFRARED}] != 0, \
                 if(pl_powers[{PW_INFRARED}] > 128 OR \
                 bitAnd(pl_powers[{PW_INFRARED}], 8) != 0, 1, 0), \
                 0))"
            ),
        ),
    ]
}

/// `P_MobjThinker` for the player's mobj: the move, then the height, then
/// the state cycle.
fn mobj_thinker(state: &State) -> Vec<(String, String)> {
    let arrays: Vec<String> = [
        "m_x",
        "m_y",
        "m_radius",
        "m_flags",
        "m_linkseq",
        "sec_floorheight",
        "sec_ceilingheight",
        "line_special",
        "m_sprite",
        "m_z",
    ]
    .iter()
    .map(|column| state.get(column))
    .collect();
    let player = inter::Player {
        health: &state.get("p_health"),
        armorpoints: &state.get("p_armorpoints"),
        armortype: &state.get("p_armortype"),
        ammo: &state.get("p_ammo"),
        maxammo: &state.get("p_maxammo"),
        backpack: &state.get("p_backpack"),
        cards: &state.get("p_cards"),
        powers: "pl_powers",
        weaponowned: &state.get("p_weaponowned"),
        pendingweapon: "psp_pendingweapon",
        message: &state.get("p_message"),
        itemcount: &state.get("p_itemcount"),
        bonuscount: "pl_bonuscount_down",
        mo_flags: "pl_flags_noclip",
    };
    let mover = Mover {
        slot: "pl_slot",
        radius: "pl_radius",
        height: "pl_height",
        z: "pl_z",
        flags: "pl_flags_noclip",
        is_player: "1",
        momx: "pl_pushx",
        momy: "pl_pushy",
        x: "pl_x",
        y: "pl_y",
        floorz: "pl_floorz",
        ceilingz: "pl_ceilingz",
        subsector: "pl_subsector",
        angle: "pl_new_angle",
        uses: "pl_uses",
    };
    let world = World {
        m_x: &arrays[0],
        m_y: &arrays[1],
        m_radius: &arrays[2],
        m_flags: &arrays[3],
        m_linkseq: &arrays[4],
        alive: &format!("move_at.{}", mobj::moving::ALIVE),
        floorheight: &arrays[5],
        ceilingheight: &arrays[6],
        line_special: &arrays[7],
    };
    let pickups = Pickups {
        m_sprite: &arrays[8],
        m_flags: &arrays[3],
        m_z: &arrays[9],
        skill: "skill",
        start: "pk0",
        alive: "mv_alive0",
    };
    let held = |field: usize| format!("mv.{field}");
    let mut bindings = vec![
        (
            "pk_readyweapon".to_owned(),
            "toInt64(now_p_readyweapon)".to_owned(),
        ),
        ("pk0".to_owned(), inter::start(&player)),
        (
            "mv_alive0".to_owned(),
            format!("arrayMap(v -> toUInt8(1), {})", arrays[0]),
        ),
        ("mv".to_owned(), mobj::xy_movement(&mover, &world, &pickups)),
        ("pk".to_owned(), held(mobj::moving::PICKED_UP)),
        ("mv_floorz".to_owned(), held(mobj::moving::FLOORZ)),
        ("mv_ceilingz".to_owned(), held(mobj::moving::CEILINGZ)),
        ("mv_subsector".to_owned(), held(mobj::moving::SUBSECTOR)),
        ("mv_x".to_owned(), held(mobj::moving::X)),
        ("mv_y".to_owned(), held(mobj::moving::Y)),
        ("mv_unfinished".to_owned(), mobj::unfinished("mv")),
        ("mv_useline".to_owned(), mobj::use_line("mv")),
        ("mv_leftx".to_owned(), held(mobj::moving::MOMX)),
        ("mv_lefty".to_owned(), held(mobj::moving::MOMY)),
        ("pk_alive".to_owned(), held(mobj::moving::ALIVE)),
    ];
    bindings.extend(mobj::friction(
        "mv_leftx",
        "mv_lefty",
        "pl_z",
        "mv_floorz",
        &state.get("p_cmd_forwardmove"),
        &state.get("p_cmd_sidemove"),
    ));
    bindings.extend(mobj::z_movement(
        "pl_z",
        "pl_momz",
        "mv_floorz",
        "mv_ceilingz",
        "pl_height",
        "pl_flags_noclip",
        "now_p_viewheight",
    ));
    bindings.extend([
        // `P_XYMovement` drops a player who has stopped out of the walking
        // frames, and the state then cycles as any thinker's does.
        (
            "mv_stopped_walking".to_owned(),
            "toUInt8(pl_pushx != 0 OR pl_pushy != 0)".to_owned(),
        ),
        (
            "mv_walked_out".to_owned(),
            "toUInt8(mv_stopped_walking = 1 AND mv_stops = 1 AND mv_airborne = 0 \
             AND toInt64(pl_state_fired) - toInt64(s_play_run1) >= 0 \
             AND toInt64(pl_state_fired) - toInt64(s_play_run1) < 4)"
                .to_owned(),
        ),
        (
            "mv_state_stopped".to_owned(),
            "toInt32(if(mv_walked_out = 1, s_play, pl_state_fired))".to_owned(),
        ),
        (
            "mv_tics_stopped".to_owned(),
            "toInt32(if(mv_walked_out = 1, state_tics[1 + s_play], pl_tics_fired))".to_owned(),
        ),
        (
            "mv_cycles".to_owned(),
            "toUInt8(mv_tics_stopped != -1 AND mv_tics_stopped - 1 = 0)".to_owned(),
        ),
        (
            "mv_state".to_owned(),
            "toInt32(if(mv_cycles = 1, state_nextstate[1 + mv_state_stopped], mv_state_stopped))"
                .to_owned(),
        ),
        (
            "mv_tics".to_owned(),
            "toInt32(multiIf(mv_cycles = 1, state_tics[1 + mv_state], \
             mv_tics_stopped = -1, -1, mv_tics_stopped - 1))"
                .to_owned(),
        ),
        // Nothing the player's own frames run has an action or a zero
        // wait, so a state that does is one this cannot carry through.
        (
            "pl_action_needed".to_owned(),
            "toUInt8(mv_cycles = 1 AND (state_action[1 + mv_state] != 0 \
             OR state_tics[1 + mv_state] = 0))"
                .to_owned(),
        ),
    ]);
    bindings.extend(super::specials::use_special_line(state, "psp_unresolved"));
    bindings
}

/// Everything the tic leaves in the state row: the player's own fields,
/// the mobj arrays with the player moved, and the list without whatever it
/// picked up.
fn writeback(state: &State) -> Vec<(String, String)> {
    // What the move left, column by column.
    let moved: Vec<(&str, &str)> = vec![
        ("m_x", "toInt32(mv_x)"),
        ("m_y", "toInt32(mv_y)"),
        ("m_z", "toInt32(mv_z)"),
        ("m_angle", "toUInt32(pl_new_angle)"),
        ("m_momx", "toInt32(mv_momx)"),
        ("m_momy", "toInt32(mv_momy)"),
        ("m_momz", "toInt32(mv_momz)"),
        ("m_floorz", "toInt32(mv_floorz)"),
        ("m_ceilingz", "toInt32(mv_ceilingz)"),
        ("m_subsector", "toInt32(mv_subsector)"),
        ("m_state", "toInt32(mv_state)"),
        ("m_tics", "toInt32(mv_tics)"),
        ("m_sprite", "toInt32(state_sprite[1 + mv_state])"),
        ("m_frame", "toInt32(state_frame[1 + mv_state])"),
        ("m_health", "toInt32(pk.1)"),
        (
            "m_flags",
            "toInt32(if(pk.14 = 1, bitOr(pl_flags_noclip, 262144), \
             bitAnd(pl_flags_noclip, -262145)))",
        ),
    ];
    let mut bindings = vec![
        // A slot that survives keeps its place, and a pointer to it moves
        // down with it.
        (
            "pk_slot".to_owned(),
            "arrayMap((a, c) -> toUInt32(if(a = 1, c, 0)), pk_alive, arrayCumSum(pk_alive))"
                .to_owned(),
        ),
    ];
    for (column, value) in &moved {
        let array = state.get(column);
        bindings.push((
            format!("moved_{column}"),
            format!(
                "arrayMap((v, k) -> if(k = pl_slot, {value}, v), {array}, arrayEnumerate({array}))"
            ),
        ));
    }
    for column in super::state_columns() {
        // `m_id` is the slot itself, which the compaction renumbers below.
        if !column.starts_with("m_") || column == "m_id" {
            continue;
        }
        let held = if moved.iter().any(|(name, _)| *name == column) {
            format!("moved_{column}")
        } else {
            state.get(column)
        };
        let held = if MOBJ_POINTERS.contains(&column) {
            renumbered(&held)
        } else {
            held
        };
        bindings.push((
            format!("now_{column}"),
            format!("arrayFilter((v, a) -> a = 1, {held}, pk_alive)"),
        ));
    }
    bindings.push((
        "now_sec_soundtarget".to_owned(),
        renumbered("nz_soundtarget"),
    ));
    bindings.extend([
        (
            "now_m_id".to_owned(),
            "arrayMap(n -> toUInt32(n), arrayEnumerate(now_m_x))".to_owned(),
        ),
        (
            "now_p_mo".to_owned(),
            "toUInt32(pk_slot[pl_slot])".to_owned(),
        ),
        ("now_p_health".to_owned(), "toInt32(pk.1)".to_owned()),
        ("now_p_armorpoints".to_owned(), "toInt32(pk.2)".to_owned()),
        ("now_p_armortype".to_owned(), "toInt32(pk.3)".to_owned()),
        ("now_p_ammo".to_owned(), "pk.4".to_owned()),
        ("now_p_maxammo".to_owned(), "pk.5".to_owned()),
        ("now_p_backpack".to_owned(), "toUInt8(pk.6)".to_owned()),
        ("now_p_cards".to_owned(), "pk.7".to_owned()),
        ("now_p_powers".to_owned(), "pk.8".to_owned()),
        ("now_p_weaponowned".to_owned(), "pk.9".to_owned()),
        // `P_GiveWeapon` puts the weapon it gave up next, so the column is
        // the pickups' answer and not the command's.
        (
            "now_p_pendingweapon".to_owned(),
            "toInt32(pk.10)".to_owned(),
        ),
        ("now_p_message".to_owned(), "toUInt64(pk.11)".to_owned()),
        ("now_p_itemcount".to_owned(), "toInt32(pk.12)".to_owned()),
        ("now_p_bonuscount".to_owned(), "toInt32(pk.13)".to_owned()),
        (
            "now_p_attacker".to_owned(),
            renumbered_slot(&state.get("p_attacker")),
        ),
    ]);
    bindings
}

/// The mobj array columns that hold a slot rather than a value of their
/// own. `sec_soundtarget` and `p_attacker` hold one too and are written
/// beside them.
const MOBJ_POINTERS: [&str; 2] = ["m_target", "m_tracer"];

/// One slot the compaction moved, as the slot it moved to. A pointer at
/// the thing that was taken becomes 0, which is what the contract says
/// none means.
fn renumbered_slot(slot: &str) -> String {
    format!("toUInt32(if({slot} = 0, 0, pk_slot[{slot}]))")
}

/// Every slot in an array of them, renumbered the same way.
fn renumbered(slots: &str) -> String {
    format!("arrayMap(t -> {}, {slots})", renumbered_slot("t"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_writes_the_player_and_the_mobjs_it_moved() {
        let bindings = think(&State::default());
        let named: Vec<&str> = bindings.iter().map(|(name, _)| name.as_str()).collect();
        for column in ["now_p_viewz", "now_p_bob", "now_m_x", "now_m_z", "now_p_mo"] {
            assert!(named.contains(&column), "{column}");
        }
        assert!(named.contains(&"now_unresolved"));
    }

    #[test]
    fn every_binding_balances_its_parentheses() {
        for (name, expr) in think(&State::default()) {
            let depth = expr.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{name}");
        }
    }
}
