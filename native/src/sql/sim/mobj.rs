//! What a thing does with its momentum and its states, from `p_mobj.c`.

use super::map::{self, World, answer};
use super::{State, enemy, inter, maputl, sight};
use crate::sql::Statement;
use crate::sql::bind;
use crate::sql::fixed;

/// `p_mobj.c`: the fastest one step carries, when the move is split.
const MAXMOVE: i64 = 30 << 16;
/// `p_mobj.c`: below this a thing that is not being pushed stops dead.
const STOPSPEED: i64 = 0x1000;
/// `p_mobj.c`: what is left of the momentum after a tic on the ground.
const FRICTION: i64 = 0xe800;
/// `p_local.h`
const GRAVITY: i64 = 1 << 16;
const VIEWHEIGHT: i64 = 41 << 16;

/// `p_mobj.h`: the z a spawn asks for when it wants the floor or the
/// ceiling it lands in.
const ONFLOORZ: i64 = i32::MIN as i64;
const ONCEILINGZ: i64 = i32::MAX as i64;
/// `d_player.h`
const MAXPLAYERS: i64 = 4;
/// `doomdef.h`: the one skill that leaves a spawned thing no reaction
/// time.
const SK_NIGHTMARE: i64 = 4;
/// `p_local.h`: how far a punch reaches, which is the range a puff sparks
/// on the wall at.
const MELEERANGE: i64 = 64 << 16;

/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
const MF_AMBUSH: i64 = 32;
const MF_NOGRAVITY: i64 = 512;
const MF_SKULLFLY: i64 = 0x100_0000;

/// Where each field of one thing's answer sits.
mod cycled {
    pub const STATE: usize = 1;
    pub const TICS: usize = 2;
    pub const TARGET: usize = 3;
    pub const THRESHOLD: usize = 4;
    pub const LASTLOOK: usize = 5;
    /// The state the thing is about to enter, or -1 for none.
    pub const PENDING: usize = 6;
    pub const STUCK: usize = 7;
    /// Whether the thing entered a state, which is what moves its picture.
    pub const MOVED: usize = 8;
}

/// The constants the thinkers read.
pub fn constants(db: &str) -> Vec<(String, String)> {
    vec![
        (
            "mobj_seestate".to_owned(),
            super::table_column(db, "mobjinfo", "seestate"),
        ),
        (
            "mobj_seesound".to_owned(),
            super::table_column(db, "mobjinfo", "seesound"),
        ),
        (
            "a_look".to_owned(),
            format!("assumeNotNull((SELECT id FROM {db}.action_functions WHERE name = 'A_Look'))"),
        ),
        (
            "mobj_spawnstate".to_owned(),
            super::table_column(db, "mobjinfo", "spawnstate"),
        ),
        (
            "mobj_spawnhealth".to_owned(),
            super::table_column(db, "mobjinfo", "spawnhealth"),
        ),
        (
            "mobj_reactiontime".to_owned(),
            super::table_column(db, "mobjinfo", "reactiontime"),
        ),
        (
            "mobj_radius".to_owned(),
            super::table_column(db, "mobjinfo", "radius"),
        ),
        (
            "mobj_height".to_owned(),
            super::table_column(db, "mobjinfo", "height"),
        ),
        (
            "mobj_flags".to_owned(),
            super::table_column(db, "mobjinfo", "flags"),
        ),
        ("mt_puff".to_owned(), thing_type(db, "MT_PUFF")),
        ("mt_blood".to_owned(), thing_type(db, "MT_BLOOD")),
        // The engine names the frames a puff and a blood spot are put into
        // by hand. No table holds them, so they are read off the spawn
        // state's own chain, which `guards` pins the length of.
        ("s_puff3".to_owned(), along_chain(db, "MT_PUFF", 2)),
        ("s_blood2".to_owned(), along_chain(db, "MT_BLOOD", 1)),
        ("s_blood3".to_owned(), along_chain(db, "MT_BLOOD", 2)),
    ]
}

/// One thing type by the name `mobjtype` holds for it.
fn thing_type(db: &str, name: &str) -> String {
    format!("assumeNotNull((SELECT toInt32(id) FROM {db}.mobjtype WHERE name = '{name}'))")
}

/// The state `hops` along the chain from `kind`'s own spawn state.
fn along_chain(db: &str, kind: &str, hops: usize) -> String {
    let mut sql = format!(
        "(SELECT spawnstate FROM {db}.mobjinfo WHERE id = {})",
        thing_type(db, kind)
    );
    for _ in 0..hops {
        sql = format!("(SELECT nextstate FROM {db}.states WHERE id = {sql})");
    }
    format!("assumeNotNull(toInt32({sql}))")
}

/// What stops the load: a puff or a blood spot whose state chain is not
/// the length the engine's own frame names assume.
///
/// `P_SpawnPuff` puts a puff into its third frame and `P_SpawnBlood` puts
/// a blood spot into its second or its third, and nothing else names them.
pub fn guards(db: &str) -> Vec<Statement> {
    let ends_at = |kind: &str, hops: usize| along_chain(db, kind, hops);
    [("MT_PUFF", 4), ("MT_BLOOD", 3)]
        .into_iter()
        .map(|(kind, length)| {
            Statement::sql(format!(
                "SELECT throwIf({} != 0 OR {} = 0, \n                     '{kind}: the spawn state chain is not {length} states long')",
                ends_at(kind, length),
                ends_at(kind, length - 1),
            ))
        })
        .collect()
}

/// `P_MobjThinker` over every thing on the list but the player's, whose
/// own thinker runs with `P_PlayerThink`.
///
/// The things here read and write nothing in common: each moves its own
/// state on, and `A_Look` reads where the player stands and what the
/// sector heard, neither of which any of them writes. So the pass is a map
/// over the list rather than a fold, and the order between them cannot be
/// seen.
///
/// These leave the tic unresolved rather than being guessed: a thing with
/// momentum or standing off its floor, a cycle that removes the thing, a
/// routine this does not run, and a cycle that wants more states than it
/// is given.
pub fn thinkers(state: &State) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let slot = s("p_mo");
    let player = |column: &str| format!("{}[{slot}]", s(column));
    let mut bindings: Vec<(String, String)> = sight::seg_openings(&sight::Heights {
        floorheight: &s("sec_floorheight"),
        ceilingheight: &s("sec_ceilingheight"),
    });
    let mut bind = |name: &str, expr: String| bindings.push((name.to_owned(), expr));

    // What the tic-start snapshot alone decides, one value per slot.
    bind("mt_slots", format!("arrayEnumerate({})", s("m_state")));
    bind(
        "mt_still",
        format!(
            "arrayMap((mx, my, mz, z, fz, fl) -> toUInt8(mx = 0 AND my = 0 AND mz = 0 \
             AND z = fz AND bitAnd(fl, {MF_SKULLFLY}) = 0), {}, {}, {}, {}, {}, {})",
            s("m_momx"),
            s("m_momy"),
            s("m_momz"),
            s("m_z"),
            s("m_floorz"),
            s("m_flags"),
        ),
    );
    bind(
        "mt_cycles",
        format!(
            "arrayMap((k, tc) -> toUInt8(k != {slot} AND tc != -1 AND tc - 1 = 0), \
             mt_slots, {})",
            s("m_tics")
        ),
    );
    bind(
        "mt_next",
        format!(
            "arrayMap(st -> toInt32(state_nextstate[1 + st]), {})",
            s("m_state")
        ),
    );
    bind(
        "mt_looks",
        "arrayMap((c, n) -> toUInt8(c = 1 AND n != 0 AND state_action[1 + n] = a_look), \
         mt_cycles, mt_next)"
            .to_owned(),
    );
    // `A_Look` reads its sector's sound target before it looks for the
    // player, and a shootable one becomes the thing's target.
    bind(
        "mt_heard",
        format!(
            "arrayMap((l, ss) -> toUInt32(if(l = 1, {}[1 + ssec_sector[1 + ss]], 0)), \
             mt_looks, {})",
            s("sec_soundtarget"),
            s("m_subsector")
        ),
    );
    bind(
        "mt_shootable",
        format!(
            "arrayMap(h -> toUInt8(h != 0 AND bitAnd({}[h], {MF_SHOOTABLE}) != 0), mt_heard)",
            s("m_flags")
        ),
    );

    // The sight checks a tic needs, batched into one call of the
    // primitive. A tic that asks nothing passes an empty list.
    //
    // A look asks about the player. A chase asks about the target the
    // thing already holds. A look whose sector heard something asks about
    // what it heard, because a thing carrying `MF_AMBUSH` wakes on that
    // only where it can see it and the chase that follows reads the same
    // answer. All three lists come out of the tic-start snapshot: a state
    // cycle enters at most one routine, so a thing that looks does not
    // chase in the same entry, and only a look moves a target.
    bind(
        "mt_lookers",
        "arrayFilter((k, l) -> l = 1, mt_slots, mt_looks)".to_owned(),
    );
    bind(
        "mt_chasers",
        format!(
            "arrayFilter((k, c, n, t) -> c = 1 AND n != 0 \
             AND state_action[1 + n] = a_chase AND t != 0, mt_slots, mt_cycles, mt_next, {})",
            s("m_target")
        ),
    );
    let pairs = |slot: &str, other: &dyn Fn(&str) -> String| {
        sight::asking(
            &format!("{}[{slot}]", s("m_subsector")),
            &format!("{}[{slot}]", s("m_x")),
            &format!("{}[{slot}]", s("m_y")),
            &format!("{}[{slot}]", s("m_z")),
            &format!("{}[{slot}]", s("m_height")),
            &other("m_subsector"),
            &other("m_x"),
            &other("m_y"),
            &other("m_z"),
            &other("m_height"),
        )
    };
    let target = |column: &str| format!("{}[{}[k]]", s(column), s("m_target"));
    bind(
        "mt_hearers",
        "arrayFilter((k, sh) -> sh = 1, mt_slots, mt_shootable)".to_owned(),
    );
    let heard = |column: &str| format!("{}[mt_heard[k]]", s(column));
    bind(
        "mt_pairs",
        format!(
            "arrayConcat(arrayMap(k -> {}, mt_lookers), arrayMap(k -> {}, mt_chasers), \
             arrayMap(k -> {}, mt_hearers))",
            pairs("k", &player),
            pairs("k", &target),
            pairs("k", &heard),
        ),
    );
    bind("mt_seen", sight::check_sight("mt_pairs"));
    // The chasers' answers, then the hearers', each behind the list before
    // it. A slot in none of the lists indexes at 0 and reads the default,
    // which is the answer a thing that asked nothing gets.
    bind(
        "mt_chase_seen",
        "arraySlice(mt_seen, 1 + length(mt_lookers), length(mt_chasers))".to_owned(),
    );
    bind(
        "mt_hearer_seen",
        "arraySlice(mt_seen, 1 + length(mt_lookers) + length(mt_chasers), length(mt_hearers))"
            .to_owned(),
    );
    bind(
        "mt_heard_seen",
        "arrayMap(k -> toUInt8(mt_hearer_seen[indexOf(mt_hearers, k)]), mt_slots)".to_owned(),
    );
    // A deaf thing wakes on what it heard only where it can see it. Every
    // other one takes it as its target and wakes, and the sight was asked
    // anyway so the chase that follows reads the target it holds.
    bind(
        "mt_hears",
        format!(
            "arrayMap((fl, sh, hs) -> toUInt8(sh = 1 \
             AND (bitAnd(fl, {MF_AMBUSH}) = 0 OR hs = 1)), {}, mt_shootable, mt_heard_seen)",
            s("m_flags")
        ),
    );
    bind(
        "mt_sees_target",
        "arrayMap((k, l, h, hs) -> toUInt8(multiIf(h = 1, hs, \
         l = 1, mt_seen[indexOf(mt_lookers, k)], \
         mt_chase_seen[indexOf(mt_chasers, k)])), \
         mt_slots, mt_looks, mt_hears, mt_heard_seen)"
            .to_owned(),
    );
    // `P_LookForPlayers`, which a look runs where its sector heard nothing
    // it wakes on.
    bind(
        "mt_finds",
        format!(
            "arrayMap((k, l, mx, my, ma) -> if(l = 1, {}, toUInt8(0)), \
             mt_slots, mt_looks, {}, {}, {})",
            enemy::look_for_players(
                "mt_seen[indexOf(mt_lookers, k)]",
                &player("m_health"),
                "mx",
                "my",
                "ma",
                &player("m_x"),
                &player("m_y"),
            ),
            s("m_x"),
            s("m_y"),
            s("m_angle"),
        ),
    );
    bind(
        "mt_wakes",
        "arrayMap((h, f) -> toUInt8(h = 1 OR f = 1), mt_hears, mt_finds)".to_owned(),
    );
    // Only the walk over the players writes `lastlook`, and a look that
    // wakes on what it heard never reaches it.
    bind(
        "mt_looked",
        "arrayMap((l, h) -> toUInt8(l = 1 AND h = 0), mt_looks, mt_hears)".to_owned(),
    );
    // What the look leaves as the target. A thing that heard something
    // shootable takes it even where the sight a deaf one needs fails, and
    // the walk over the players overwrites it where that finds one.
    bind(
        "mt_target",
        format!(
            "arrayMap((l, sh, h, f, hd, tg) -> toUInt32(multiIf(l = 0, tg, \
             h = 1, hd, f = 1, {slot}, sh = 1, hd, tg)), \
             mt_looks, mt_shootable, mt_hears, mt_finds, mt_heard, {})",
            s("m_target")
        ),
    );

    // `P_SetMobjState`, unrolled twice. Each entry sets the state, its
    // wait and its picture, then runs the routine the state carries. The
    // first is the state the tic count ran out on. `A_Look` is the only
    // routine written here that puts the thing somewhere else, and its
    // see state waits tics of its own, so nothing reaches a third; a
    // cycle that wants one says the tic could not be produced.
    bind(
        "mt_one",
        format!(
            "arrayMap((k, c, n, w, l, lk, tt, still, st, tc, th, ll, ty) -> ({}), \
             mt_slots, mt_cycles, mt_next, mt_wakes, mt_looks, mt_looked, mt_target, mt_still, \
             {}, {}, {}, {}, {})",
            entry_one(&slot),
            s("m_state"),
            s("m_tics"),
            s("m_threshold"),
            s("m_lastlook"),
            s("m_type"),
        ),
    );
    bind(
        "mt_two",
        format!("arrayMap(a -> ({}), mt_one)", entry_two()),
    );

    // What the state cycle leaves. The chase below reads the target and
    // the threshold it left and writes the threshold again.
    let read = |member: usize, cast: &str| format!("arrayMap(a -> {cast}(a.{member}), mt_two)");
    bind("now_m_state", read(cycled::STATE, "toInt32"));
    bind("now_m_tics", read(cycled::TICS, "toInt32"));
    bind("now_m_target", read(cycled::TARGET, "toUInt32"));
    bind("mt_threshold", read(cycled::THRESHOLD, "toInt32"));
    bind("now_m_lastlook", read(cycled::LASTLOOK, "toInt32"));
    for (column, table) in [("m_sprite", "state_sprite"), ("m_frame", "state_frame")] {
        bind(
            &format!("now_{column}"),
            format!(
                "arrayMap((a, v) -> toInt32(if(a.{} = 1, {table}[1 + a.{}], v)), mt_two, {})",
                cycled::MOVED,
                cycled::STATE,
                s(column)
            ),
        );
    }
    // A thing that takes the player as its target plays the sound it makes
    // on seeing one, and two arms of that switch draw. Nothing reads the
    // number, so what the pass carries out of it is how many were drawn.
    bind(
        "mt_shouts",
        format!(
            "arrayMap((l, w, ty) -> toUInt8(l = 1 AND w = 1 AND {} = 1), \
             mt_looks, mt_wakes, {})",
            enemy::see_sound_draws("mobj_seesound[1 + ty]"),
            s("m_type")
        ),
    );

    // `A_Chase` runs inside the `P_SetMobjState` that entered the state
    // carrying it, so a thing that wakes chases on the same tic.
    bind(
        "mt_entries",
        format!(
            "arrayMap(a -> toUInt8(if(a.{moved} = 1 \
             AND state_action[1 + a.{state}] = a_chase, 1, 0) \
             + if(a.{pending} != -1 AND state_action[1 + a.{pending}] = a_chase, 1, 0)), mt_one)",
            moved = cycled::MOVED,
            state = cycled::STATE,
            pending = cycled::PENDING,
        ),
    );
    bind(
        "mt_movers",
        "arrayFilter((k, e) -> e > 0, mt_slots, mt_entries)".to_owned(),
    );
    bind(
        "mt_alive",
        format!("arrayMap(v -> toUInt8(1), {})", s("m_x")),
    );
    let world = World {
        m_x: &s("m_x"),
        m_y: &s("m_y"),
        m_radius: &s("m_radius"),
        m_flags: &s("m_flags"),
        m_linkseq: &s("m_linkseq"),
        alive: "mt_alive",
        floorheight: &s("sec_floorheight"),
        ceilingheight: &s("sec_ceilingheight"),
        line_special: &s("line_special"),
    };
    let chasing = enemy::Chasing {
        movers: "mt_movers",
        entries: "mt_entries",
        shouts: "mt_shouts",
        m_x: &s("m_x"),
        m_y: &s("m_y"),
        m_z: &s("m_z"),
        m_angle: &s("m_angle"),
        m_radius: &s("m_radius"),
        m_height: &s("m_height"),
        m_flags: &s("m_flags"),
        m_type: &s("m_type"),
        m_health: &s("m_health"),
        m_target: "now_m_target",
        m_movedir: &s("m_movedir"),
        m_movecount: &s("m_movecount"),
        m_reactiontime: &s("m_reactiontime"),
        m_threshold: "mt_threshold",
        m_floorz: &s("m_floorz"),
        m_ceilingz: &s("m_ceilingz"),
        m_subsector: &s("m_subsector"),
        sees_target: "mt_sees_target",
        prndindex: &s("prndindex"),
    };
    for (name, expr) in enemy::chase(&chasing, &world) {
        bind(&name, expr);
    }
    // What the chase left, put back where the mover stands, as one value
    // per slot. A slot no mover holds keeps what the cycle left it, so the
    // answers below read this and not the movers' own list.
    // Every column the chase writes, in the order its answer names them,
    // and where a slot no mover holds takes its value from.
    let held: [(&str, usize, &str, String); 11] = [
        ("m_x", enemy::chased::X, "toInt32", s("m_x")),
        ("m_y", enemy::chased::Y, "toInt32", s("m_y")),
        ("m_z", enemy::chased::Z, "toInt32", s("m_z")),
        ("m_angle", enemy::chased::ANGLE, "toUInt32", s("m_angle")),
        (
            "m_movedir",
            enemy::chased::MOVEDIR,
            "toInt32",
            s("m_movedir"),
        ),
        (
            "m_movecount",
            enemy::chased::MOVECOUNT,
            "toInt32",
            s("m_movecount"),
        ),
        (
            "m_reactiontime",
            enemy::chased::REACTIONTIME,
            "toInt32",
            s("m_reactiontime"),
        ),
        (
            "m_threshold",
            enemy::chased::THRESHOLD,
            "toInt32",
            "mt_threshold".to_owned(),
        ),
        ("m_floorz", enemy::chased::FLOORZ, "toInt32", s("m_floorz")),
        (
            "m_ceilingz",
            enemy::chased::CEILINGZ,
            "toInt32",
            s("m_ceilingz"),
        ),
        (
            "m_subsector",
            enemy::chased::SUBSECTOR,
            "toInt32",
            s("m_subsector"),
        ),
    ];
    let mut standing: Vec<String> = vec![String::new(); enemy::chased::STUCK];
    for (_, member, cast, array) in &held {
        standing[member - 1] = format!("{cast}({array}[k])");
    }
    standing[enemy::chased::DRAWS - 1] = "toUInt32(0)".to_owned();
    standing[enemy::chased::STUCK - 1] = "toUInt8(0)".to_owned();
    // What the chase left, put back where the mover stands, as one value
    // per slot. A slot no mover holds keeps what the cycle left it.
    bind(
        "cw_slot",
        format!(
            "arrayMap(k -> if(indexOf(mt_movers, k) = 0, ({}), \
             cw_chased[indexOf(mt_movers, k)]), arrayEnumerate({}))",
            standing.join(", "),
            s("m_x")
        ),
    );
    for (column, member, cast, _) in &held {
        bind(
            &format!("now_{column}"),
            format!("arrayMap(c -> {cast}(c.{member}), cw_slot)"),
        );
    }
    bind(
        "now_prndindex",
        format!(
            "toUInt8(bitAnd(toUInt32({}) + arraySum(mt_shouts) \
             + arraySum(arrayMap(c -> c.{}, cw_chased)), 255))",
            s("prndindex"),
            enemy::chased::DRAWS
        ),
    );
    bind(
        "now_unresolved",
        format!(
            "toUInt8({} = 1 OR arrayExists(a -> a.{} = 1, mt_two) \
             OR arrayExists(c -> c.{} = 1, cw_chased) OR cw_crowded = 1)",
            s("unresolved"),
            cycled::STUCK,
            enemy::chased::STUCK
        ),
    );
    bindings
}

/// The first state a cycle enters, and `A_Look` where the state carries
/// it.
fn entry_one(slot: &str) -> String {
    let enters = "c = 1 AND n != 0";
    let members = [
        format!("toInt32(if({enters}, n, st))"),
        // `P_MobjThinker` drops the count, and `P_SetMobjState` writes the
        // entered state's own over it.
        format!(
            "toInt32(multiIf({enters}, state_tics[1 + n], \
             k = {slot} OR tc = -1, tc, tc - 1))"
        ),
        "toUInt32(tt)".to_owned(),
        "toInt32(if(l = 1, 0, th))".to_owned(),
        format!("toInt32(if(lk = 1, {}, ll))", enemy::LASTLOOK),
        format!(
            "toInt32(multiIf(NOT ({enters}), -1, \
             l = 1 AND w = 1, mobj_seestate[1 + ty], \
             state_tics[1 + n] = 0, state_nextstate[1 + n], -1))"
        ),
        format!(
            "toUInt8(multiIf(k = {slot}, 0, still = 0, 1, c = 0, 0, n = 0, 1, \
             state_action[1 + n] != 0 AND state_action[1 + n] != a_look \
             AND state_action[1 + n] != a_chase, 1, 0))"
        ),
        format!("toUInt8({enters})"),
    ];
    members.join(", ")
}

/// The state a routine sent the cycle on to. Nothing written here carries
/// a routine of its own, so entering one says the tic could not be
/// produced.
fn entry_two() -> String {
    let held = |member: usize| format!("a.{member}");
    let enters = format!("a.{} != -1", cycled::PENDING);
    let entering = format!("a.{}", cycled::PENDING);
    let members = [
        format!("toInt32(if({enters}, {entering}, {}))", held(cycled::STATE)),
        format!(
            "toInt32(if({enters}, state_tics[1 + {entering}], {}))",
            held(cycled::TICS)
        ),
        held(cycled::TARGET),
        held(cycled::THRESHOLD),
        held(cycled::LASTLOOK),
        "toInt32(-1)".to_owned(),
        format!(
            "toUInt8({} = 1 OR ({enters} AND ({entering} = 0 \
             OR (state_action[1 + {entering}] != 0 \
             AND state_action[1 + {entering}] != a_chase) \
             OR state_tics[1 + {entering}] = 0)))",
            held(cycled::STUCK)
        ),
        format!("toUInt8({enters} OR a.{} = 1)", cycled::MOVED),
    ];
    members.join(", ")
}

/// Where each field of the move loop's state sits in its tuple.
pub mod moving {
    pub const X: usize = 1;
    pub const Y: usize = 2;
    pub const XMOVE: usize = 3;
    pub const YMOVE: usize = 4;
    pub const PHASE: usize = 5;
    pub const FLOORZ: usize = 6;
    pub const CEILINGZ: usize = 7;
    pub const SUBSECTOR: usize = 8;
    pub const HITCOUNT: usize = 9;
    pub const PICKED_UP: usize = 10;
    pub const ALIVE: usize = 11;
    pub const MOMX: usize = 12;
    pub const MOMY: usize = 13;
    pub const SLIDEX: usize = 14;
    pub const SLIDEY: usize = 15;
    pub const USELINE: usize = 16;
}

/// The thing whose momentum is being spent, as expressions.
pub struct Mover<'a> {
    pub slot: &'a str,
    pub radius: &'a str,
    pub height: &'a str,
    pub z: &'a str,
    pub flags: &'a str,
    pub is_player: &'a str,
    /// The momentum the push left, before the loop clamps it.
    pub momx: &'a str,
    pub momy: &'a str,
    pub x: &'a str,
    pub y: &'a str,
    pub floorz: &'a str,
    pub ceilingz: &'a str,
    pub subsector: &'a str,
    /// The angle the command has already turned the thing to.
    pub angle: &'a str,
    /// 1 on the tic the use key goes down.
    pub uses: &'a str,
}

/// What a pickup needs to read and to leave behind.
pub struct Pickups<'a> {
    pub m_sprite: &'a str,
    pub m_flags: &'a str,
    pub m_z: &'a str,
    pub skill: &'a str,
    /// The accumulator the first pickup starts from.
    pub start: &'a str,
    /// Every mobj slot, all alive, as the loop starts.
    pub alive: &'a str,
}

/// Where the loop has got to, which is what the engine's `goto` looks
/// like from outside the function.
mod phase {
    /// `P_XYMovement`'s own loop: try the next part of the move.
    pub const STEP: i64 = 0;
    /// `P_SlideMove`: trace the three leading corners and move up to the
    /// nearest wall any of them hits.
    pub const SLIDE: i64 = 1;
    /// Slide the rest of the way along that wall.
    pub const ALONG: i64 = 2;
    /// `stairstep`: try the two axes on their own.
    pub const STAIR_Y: i64 = 3;
    pub const STAIR_X: i64 = 4;
    /// Nothing left to do.
    pub const DONE: i64 = 5;
    pub const USE: i64 = 6;
}

/// `p_map.c`: how many walls a slide bounces off before it stair-steps.
const SLIDE_TRIES: i64 = 3;
/// `p_map.c`: how far short of the wall the slide stops.
const SLIDE_NUDGE: i64 = 0x800;
/// `m_fixed.h`
const FRACUNIT: i64 = 1 << 16;
/// `tables.h`
const ANG180: i64 = 0x8000_0000;
const ANGLETOFINESHIFT: u32 = 19;
/// `doomdata.h`
const ML_TWOSIDED: i64 = 4;
/// `r_defs.h`
const ST_HORIZONTAL: i64 = 0;
const ST_VERTICAL: i64 = 1;
/// `p_map.c`
const MAXSTEP: i64 = 24 << 16;
/// `p_local.h`: how far in front of itself a thing reaches to use a line,
/// in whole units, which is how `P_UseLines` scales the direction.
const USERANGE: i64 = 64;

/// How many steps past the move itself the loop is given for the slide.
///
/// `P_SlideMove` counts a try before it scans, so the last of them stair
/// steps without scanning. Each of the tries before it is a move up to the
/// wall and a move along it, and the stair step is two more.
const SLIDE_BUDGET: i64 = 2 * (SLIDE_TRIES - 1) + 2;

/// How many steps the loop is given.
///
/// A move nothing blocks takes one step or two, because halving once puts
/// both axes under half of `MAXMOVE`. What is left is the slide's budget,
/// and a tic that wants more than that says it could not be produced.
pub fn steps(momx: &str, momy: &str, uses: &str) -> String {
    format!(
        "toUInt32(if({uses} = 1, 1, 0) + multiIf({momx} = 0 AND {momy} = 0, 0, \
         {clamped_x} > {half} OR {clamped_y} > {half}, {}, {}))",
        2 + 2 * SLIDE_BUDGET,
        1 + SLIDE_BUDGET,
        half = MAXMOVE / 2,
        clamped_x = clamp(momx),
        clamped_y = clamp(momy),
    )
}

/// `P_XYMovement` and `P_SlideMove`, as one fold whose accumulator carries
/// where the thing has got to and which part of the two it is in.
///
/// The steps depend on each other, so the fold is the loop: each reads the
/// position the one before it reached and the world it left, because
/// `P_TouchSpecialThing` takes a thing off the blockmap as it picks it up.
/// `P_TryMove` and `P_PathTraverse` appear once each, and the phase
/// decides what they are asked. A phase that asks nothing passes an empty
/// array, which costs nothing to walk.
pub fn xy_movement(mover: &Mover<'_>, world: &World<'_>, pickups: &Pickups<'_>) -> String {
    let held = |field: usize| format!("move_at.{field}");
    let at = |p: i64| format!("({} = {p})", held(moving::PHASE));
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));
    let fixed_mul = |a: &str, b: &str| format!("bitShiftRight(toInt64({a}) * toInt64({b}), 16)");

    // The scan: the three leading corners of the box, each traced along
    // the whole of what is left to move.
    let corner = |mom: usize, coord: usize, lead: bool| {
        let sign = if lead { "" } else { "-" };
        format!(
            "toInt64({} {sign}+ if({} > 0, toInt64({r}), -toInt64({r})))",
            held(coord),
            held(mom),
            r = mover.radius
        )
        .replace("-+", "-")
    };
    value("sl_leadx", corner(moving::MOMX, moving::X, true));
    value("sl_leady", corner(moving::MOMY, moving::Y, true));
    value("sl_trailx", corner(moving::MOMX, moving::X, false));
    value("sl_traily", corner(moving::MOMY, moving::Y, false));
    let trace = |x: &str, y: &str| {
        maputl::tracing(
            x,
            y,
            &format!("{x} + {}", held(moving::MOMX)),
            &format!("{y} + {}", held(moving::MOMY)),
        )
    };
    // `P_UseLines` reaches straight ahead from where the thing stands.
    value(
        "sl_fine",
        format!(
            "toUInt32(bitShiftRight(toInt64({}), {ANGLETOFINESHIFT}))",
            mover.angle
        ),
    );
    let reach = |wave: String, coord: usize| {
        format!("toInt64({} + {USERANGE} * toInt64({wave}))", held(coord))
    };
    value(
        "sl_hits",
        maputl::path_traverse(
            &format!(
                "multiIf({slide}, [{}, {}, {}], {use}, [{}], \
             CAST([], 'Array(Tuple(Int64, Int64, Int64, Int64))'))",
                trace("sl_leadx", "sl_leady"),
                trace("sl_trailx", "sl_leady"),
                trace("sl_leadx", "sl_traily"),
                maputl::tracing(
                    &held(moving::X),
                    &held(moving::Y),
                    &reach(maputl::finecosine("sl_fine"), moving::X),
                    &reach(maputl::finesine("sl_fine"), moving::Y),
                ),
                slide = at(phase::SLIDE),
                r#use = at(phase::USE),
            ),
            None,
        ),
    );
    value(
        "sl_blocking",
        blocking(mover, world, &held, &at(phase::USE)),
    );
    // The nearest wall any of the three traces found. The engine walks them
    // in order and keeps the first of an equal pair, so a stable sort by
    // fraction puts the engine's choice first.
    value(
        "sl_nearest",
        format!(
            "arrayFirst(h -> 1, arrayPushBack(arraySort(h -> h.2, sl_blocking), \
             (toInt32(-1), toInt32({}), toUInt8(1))))",
            FRACUNIT + 1
        ),
    );
    value("sl_bestfrac", "toInt64(sl_nearest.2)".to_owned());
    value("sl_bestline", "toInt64(sl_nearest.1)".to_owned());
    value(
        "sl_upto",
        format!("toInt64(greatest(sl_bestfrac - {SLIDE_NUDGE}, 0))"),
    );
    value(
        "sl_left",
        format!(
            "toInt64(least({FRACUNIT} - (sl_bestfrac - {SLIDE_NUDGE} + {SLIDE_NUDGE}), {FRACUNIT}))"
        ),
    );
    value("sl_movex", fixed_mul(&held(moving::MOMX), "sl_left"));
    value("sl_movey", fixed_mul(&held(moving::MOMY), "sl_left"));
    for (name, expr) in hit_slide_line("sl_movex", "sl_movey", &held) {
        value(&name, expr);
    }

    // What `P_XYMovement`'s own loop is trying.
    value(
        "st_split",
        format!(
            "toUInt8({} AND ({} > {half} OR {} > {half}))",
            at(phase::STEP),
            held(moving::XMOVE),
            held(moving::YMOVE),
            half = MAXMOVE / 2
        ),
    );
    value(
        "st_dx",
        format!(
            "toInt64(if(st_split = 1, intDiv({}, 2), {}))",
            held(moving::XMOVE),
            held(moving::XMOVE)
        ),
    );
    value(
        "st_dy",
        format!(
            "toInt64(if(st_split = 1, intDiv({}, 2), {}))",
            held(moving::YMOVE),
            held(moving::YMOVE)
        ),
    );
    value(
        "st_tryx",
        format!(
            "toInt32(multiIf({step}, {x} + st_dx, \
             {slide}, {x} + {}, \
             {along}, {x} + {slidex}, \
             {stair_x}, {x} + {momx}, {x}))",
            fixed_mul(&held(moving::MOMX), "sl_upto"),
            x = held(moving::X),
            momx = held(moving::MOMX),
            slidex = held(moving::SLIDEX),
            step = at(phase::STEP),
            slide = at(phase::SLIDE),
            along = at(phase::ALONG),
            stair_x = at(phase::STAIR_X),
        ),
    );
    value(
        "st_tryy",
        format!(
            "toInt32(multiIf({step}, {y} + st_dy, \
             {slide}, {y} + {}, \
             {along}, {y} + {slidey}, \
             {stair_y}, {y} + {momy}, {y}))",
            fixed_mul(&held(moving::MOMY), "sl_upto"),
            y = held(moving::Y),
            momy = held(moving::MOMY),
            slidey = held(moving::SLIDEY),
            step = at(phase::STEP),
            slide = at(phase::SLIDE),
            along = at(phase::ALONG),
            stair_y = at(phase::STAIR_Y),
        ),
    );
    // The scan itself moves the thing only when it found a wall to move up
    // to; a scan that found none goes straight to the stair step.
    value(
        "st_asks",
        format!(
            "toUInt8(NOT {done} AND NOT {use} \
             AND NOT ({slide} AND (sl_bestfrac > {FRACUNIT} OR sl_upto <= 0)))",
            done = at(phase::DONE),
            slide = at(phase::SLIDE),
            r#use = at(phase::USE),
        ),
    );
    let asking = map::asking(
        mover.slot,
        "st_tryx",
        "st_tryy",
        mover.radius,
        mover.height,
        mover.z,
        mover.flags,
        mover.is_player,
    );
    value(
        "st_answers",
        map::try_moves(
            &format!(
                "if(st_asks = 1, [{asking}], \
                 CAST([], 'Array(Tuple(UInt32, Int32, Int32, Int32, Int32, Int32, Int32, UInt8))'))"
            ),
            world,
        ),
    );
    value(
        "st_ok",
        format!(
            "toUInt8(st_asks = 1 AND arrayFirst(a -> 1, st_answers).{} = 1)",
            answer::OK
        ),
    );
    value(
        "st_picked",
        format!(
            "if(st_asks = 1, arrayFirst(a -> 1, st_answers).{}, CAST([], 'Array(UInt32)'))",
            answer::PICKED
        ),
    );
    value(
        "st_pk",
        inter::touch(
            "st_picked",
            &held(moving::PICKED_UP),
            pickups.m_sprite,
            pickups.m_flags,
            pickups.m_z,
            mover.z,
            mover.height,
            pickups.skill,
        ),
    );
    // `P_XYMovement`'s own loop runs again while a split move has a half
    // left, whether the slide took the last one over or not.
    value(
        "st_resume",
        format!(
            "toInt64(if({} != 0 OR {} != 0, {}, {}))",
            held(moving::XMOVE),
            held(moving::YMOVE),
            phase::STEP,
            phase::DONE
        ),
    );
    // Where the loop goes next.
    value(
        "st_next",
        format!(
            "toInt64(multiIf(\
             {done}, {DONE}, \
             {use} AND ({momx} != 0 OR {momy} != 0), {STEP}, \
             {use}, {DONE}, \
             {step} AND st_ok = 1 AND st_split = 1, {STEP}, \
             {step} AND st_ok = 1, {DONE}, \
             {step}, {SLIDE}, \
             {slide} AND sl_bestfrac > {FRACUNIT}, {STAIR_Y}, \
             {slide} AND st_asks = 1 AND st_ok = 0, {STAIR_Y}, \
             {slide} AND sl_left <= 0, {left}, \
             {slide}, {ALONG}, \
             {along} AND st_ok = 1, {left}, \
             {along} AND {hits} + 1 >= {SLIDE_TRIES}, {STAIR_Y}, \
             {along}, {SLIDE}, \
             {stair_y} AND st_ok = 1, {left}, \
             {stair_y}, {STAIR_X}, \
             {left}))",
            DONE = phase::DONE,
            STEP = phase::STEP,
            SLIDE = phase::SLIDE,
            ALONG = phase::ALONG,
            STAIR_Y = phase::STAIR_Y,
            STAIR_X = phase::STAIR_X,
            done = at(phase::DONE),
            step = at(phase::STEP),
            slide = at(phase::SLIDE),
            along = at(phase::ALONG),
            stair_y = at(phase::STAIR_Y),
            r#use = at(phase::USE),
            momx = held(moving::MOMX),
            momy = held(moving::MOMY),
            left = "st_resume",
            hits = held(moving::HITCOUNT),
        ),
    );
    let keep = |field: usize, when_moved: String| {
        format!("toInt32(if(st_ok = 1, {when_moved}, {}))", held(field))
    };
    let answered = |field: usize| format!("arrayFirst(a -> 1, st_answers).{field}");
    // The accumulator's members, in the order `moving` names them.
    // `P_XYMovement` halves before it tries the move, so a split move that
    // the slide takes over still has its second half to spend.
    let halved = |field: usize| {
        format!(
            "toInt64(multiIf(NOT {step}, {held}, \
             st_split = 1, bitShiftRight({held}, 1), 0))",
            step = at(phase::STEP),
            held = held(field)
        )
    };
    // `P_SlideMove` keeps what it clipped the move down to. The scan is
    // the only phase that traces, so the vector it clipped has to keep
    // until the phase after it tries the move.
    let clipped = |kept: usize, when: &str, from: usize| {
        format!("toInt64(if({when}, {}, {}))", held(from), held(kept))
    };
    let members = [
        keep(moving::X, "st_tryx".to_owned()),
        keep(moving::Y, "st_tryy".to_owned()),
        halved(moving::XMOVE),
        halved(moving::YMOVE),
        "st_next".to_owned(),
        keep(moving::FLOORZ, answered(answer::FLOORZ)),
        keep(moving::CEILINGZ, answered(answer::CEILINGZ)),
        keep(moving::SUBSECTOR, answered(answer::SUBSECTOR)),
        // Each blocked move gets its own three tries at the wall.
        format!(
            "toInt64(if({step}, 0, {} + if({along} AND st_ok = 0, 1, 0)))",
            held(moving::HITCOUNT),
            step = at(phase::STEP),
            along = at(phase::ALONG)
        ),
        "st_pk".to_owned(),
        format!(
            "arrayMap((a, k) -> toUInt8(if(has(st_pk.{}, toUInt32(k)), 0, a)), {held_alive}, \
             arrayEnumerate({held_alive}))",
            inter::TAKEN,
            held_alive = held(moving::ALIVE)
        ),
        clipped(moving::MOMX, &at(phase::ALONG), moving::SLIDEX),
        clipped(moving::MOMY, &at(phase::ALONG), moving::SLIDEY),
        format!(
            "toInt64(if({slide}, sl_slidex, {}))",
            held(moving::SLIDEX),
            slide = at(phase::SLIDE)
        ),
        format!(
            "toInt64(if({slide}, sl_slidey, {}))",
            held(moving::SLIDEY),
            slide = at(phase::SLIDE)
        ),
        // `PTR_UseTraverse` stops at the first line it cannot see past. A
        // line with a special on it is the one the press acts on; one
        // without is a wall the press does not reach through.
        format!(
            "toInt64(if({use}, if(sl_bestline >= 0 AND {}[1 + sl_bestline] != 0, \
             sl_bestline, -1), {}))",
            world.line_special,
            held(moving::USELINE),
            r#use = at(phase::USE)
        ),
    ];
    let body = format!("({})", members.join(", "));
    let start = format!(
        "(toInt32({x}), toInt32({y}), {xmove}, {ymove}, \
         toInt64(multiIf({uses} = 1, {USE}, {momx} != 0 OR {momy} != 0, {STEP}, {DONE})), \
         toInt32({floorz}), toInt32({ceilingz}), toInt32({subsector}), toInt64(0), {pk}, {alive}, \
         toInt64({xmove}), toInt64({ymove}), toInt64(0), toInt64(0), toInt64(-1))",
        USE = phase::USE,
        STEP = phase::STEP,
        DONE = phase::DONE,
        x = mover.x,
        y = mover.y,
        xmove = format!("toInt64({})", clamp(mover.momx)),
        ymove = format!("toInt64({})", clamp(mover.momy)),
        momx = mover.momx,
        momy = mover.momy,
        floorz = mover.floorz,
        ceilingz = mover.ceilingz,
        subsector = mover.subsector,
        uses = mover.uses,
        pk = pickups.start,
        alive = pickups.alive,
    );
    format!(
        "arrayFold((move_at, move_step) -> {}, range({}), {start})",
        bind::chain(&values, &body),
        steps(mover.momx, mover.momy, mover.uses)
    )
}

/// Whether the loop ran out of steps before it was finished, which is a
/// tic the simulation could not produce in full.
/// The special line `P_UseLines` reached, or -1.
pub fn use_line(loop_state: &str) -> String {
    format!("toInt64({loop_state}.{})", moving::USELINE)
}

pub fn unfinished(loop_state: &str) -> String {
    format!("toUInt8({loop_state}.{} != {})", moving::PHASE, phase::DONE)
}

/// `PTR_SlideTraverse`: the first line of each trace that stops the thing,
/// with the fraction along the trace it stopped at.
fn blocking(
    mover: &Mover<'_>,
    world: &World<'_>,
    held: &dyn Fn(usize) -> String,
    is_use: &str,
) -> String {
    let line = format!("h.{}", maputl::intercept::ID);
    // The opening is read four times, so it is bound once inside the
    // lambda rather than written out at each of them.
    let stops = bind::chain(
        &[(
            "op".to_owned(),
            maputl::opening(&line, world.floorheight, world.ceilingheight),
        )],
        &format!(
            "if({is_use}, \
             {}[1 + {line}] != 0 OR op.1 - op.2 <= 0, \
             if(bitAnd(line_flags[1 + {line}], {ML_TWOSIDED}) = 0, \
             {} = 0, \
             op.1 - op.2 < toInt64({height}) \
             OR op.1 - toInt64({z}) < toInt64({height}) \
             OR op.2 - toInt64({z}) > {MAXSTEP}))",
            world.line_special,
            map::point_on_line_side(
                &format!("toInt32({})", held(moving::X)),
                &format!("toInt32({})", held(moving::Y)),
                &line
            ),
            height = mover.height,
            z = mover.z,
        ),
    );
    // A trace that reaches nothing ends on the sentinel, which no line
    // can beat and the caller drops.
    format!(
        "arrayFilter(h -> h.2 <= {FRACUNIT}, arrayMap(hits -> \
         arrayFirst(h -> 1, arrayPushBack(arrayFilter(h -> {stops}, hits), \
         (toInt32(-1), toInt32({}), toUInt8(1)))), sl_hits))",
        FRACUNIT + 1
    )
}

/// `P_HitSlideLine`: what is left of the move, turned to run along the
/// wall it hit, as the values that build `sl_slidex` and `sl_slidey`.
///
/// The two axes share every angle and length between them, so each one is
/// a value of its own and the pair reads it.
fn hit_slide_line(
    movex: &str,
    movey: &str,
    held: &dyn Fn(usize) -> String,
) -> Vec<(String, String)> {
    let line = "sl_bestline";
    let slope = format!("line_slopetype[1 + {line}]");
    let mut values: Vec<(String, String)> = Vec::new();
    let mut value = |name: &str, expr: String| values.push((name.to_owned(), expr));

    value(
        "sl_side",
        map::point_on_line_side(
            &format!("toInt32({})", held(moving::X)),
            &format!("toInt32({})", held(moving::Y)),
            line,
        ),
    );
    value(
        "sl_lineangle",
        format!(
            "toUInt32(bitAnd(toInt64({}) + if(sl_side = 1, {ANG180}, 0), 4294967295))",
            fixed::point_to_angle(
                &format!("line_dx[1 + {line}]"),
                &format!("line_dy[1 + {line}]"),
                "tantoangle"
            )
        ),
    );
    value(
        "sl_moveangle",
        fixed::point_to_angle(
            &format!("toInt32({movex})"),
            &format!("toInt32({movey})"),
            "tantoangle",
        ),
    );
    value(
        "sl_delta",
        "toUInt32(bitAnd(toInt64(sl_moveangle) - toInt64(sl_lineangle) + 4294967296, \
         4294967295))"
            .to_owned(),
    );
    value(
        "sl_deltafine",
        format!(
            "toUInt32(bitShiftRight(bitAnd(if(sl_delta > {ANG180}, \
             toInt64(sl_delta) + {ANG180}, toInt64(sl_delta)), 4294967295), {ANGLETOFINESHIFT}))"
        ),
    );
    value(
        "sl_linefine",
        format!("toUInt32(bitShiftRight(toInt64(sl_lineangle), {ANGLETOFINESHIFT}))"),
    );
    value(
        "sl_movelen",
        fixed::aprox_distance(&format!("toInt32({movex})"), &format!("toInt32({movey})")),
    );
    value(
        "sl_newlen",
        fixed::fixed_mul("toInt32(sl_movelen)", &maputl::finecosine("sl_deltafine")),
    );
    // A wall square to an axis keeps the move on that axis; every other
    // wall takes the length turned to run along it.
    let axis = |along: String, flat: &str, upright: &str, whole: &str| {
        format!(
            "toInt64(multiIf({line} < 0, toInt64({whole}), \
             {slope} = {ST_HORIZONTAL}, toInt64({flat}), \
             {slope} = {ST_VERTICAL}, toInt64({upright}), \
             toInt64({along})))"
        )
    };
    value(
        "sl_slidex",
        axis(
            fixed::fixed_mul("toInt32(sl_newlen)", &maputl::finecosine("sl_linefine")),
            movex,
            "0",
            movex,
        ),
    );
    value(
        "sl_slidey",
        axis(
            fixed::fixed_mul("toInt32(sl_newlen)", &maputl::finesine("sl_linefine")),
            "0",
            movey,
            movey,
        ),
    );
    values
}

/// `P_XYMovement` clamps each axis to `MAXMOVE` before it starts.
fn clamp(mom: &str) -> String {
    format!("toInt32(least(greatest(toInt64({mom}), -{MAXMOVE}), {MAXMOVE}))")
}

/// The friction `P_XYMovement` applies once the move is done.
///
/// A player who is still pressing a key keeps sliding; one who is not and
/// is under `STOPSPEED` stops dead and drops out of the walking frames.
pub fn friction(
    momx: &str,
    momy: &str,
    z: &str,
    floorz: &str,
    forwardmove: &str,
    sidemove: &str,
) -> Vec<(String, String)> {
    let airborne = format!("toInt64({z}) > toInt64({floorz})");
    let stops = format!(
        "{momx} > -{STOPSPEED} AND {momx} < {STOPSPEED} \
         AND {momy} > -{STOPSPEED} AND {momy} < {STOPSPEED} \
         AND {forwardmove} = 0 AND {sidemove} = 0"
    );
    let slowed = |mom: &str| format!("toInt32(bitShiftRight(toInt64({mom}) * {FRICTION}, 16))");
    vec![
        ("mv_airborne".to_owned(), format!("toUInt8({airborne})")),
        ("mv_stops".to_owned(), format!("toUInt8({stops})")),
        (
            "mv_momx".to_owned(),
            format!(
                "toInt32(multiIf(mv_airborne = 1, {momx}, mv_stops = 1, 0, {}))",
                slowed(momx)
            ),
        ),
        (
            "mv_momy".to_owned(),
            format!(
                "toInt32(multiIf(mv_airborne = 1, {momy}, mv_stops = 1, 0, {}))",
                slowed(momy)
            ),
        ),
    ]
}

/// `P_ZMovement` for a thing with no float and no missile: the height
/// moves, the floor and the ceiling clip it, and gravity pulls it down
/// when it is above the floor.
pub fn z_movement(
    z: &str,
    momz: &str,
    floorz: &str,
    ceilingz: &str,
    height: &str,
    flags: &str,
    viewheight: &str,
) -> Vec<(String, String)> {
    vec![
        // A player walking up a step has its view lowered and raised back.
        (
            "mv_stepup".to_owned(),
            format!("toInt64({floorz}) - toInt64({z})"),
        ),
        (
            "mv_viewheight_step".to_owned(),
            format!(
                "toInt32(if(mv_stepup > 0, toInt64({viewheight}) - mv_stepup, toInt64({viewheight})))"
            ),
        ),
        (
            "mv_deltaviewheight_step".to_owned(),
            format!(
                "toInt32(if(mv_stepup > 0, \
                 bitShiftRight({VIEWHEIGHT} - toInt64(mv_viewheight_step), 3), toInt64(0)))"
            ),
        ),
        (
            "mv_zstepped".to_owned(),
            format!("toInt64({z}) + toInt64({momz})"),
        ),
        (
            "mv_onfloor".to_owned(),
            format!("toUInt8(mv_zstepped <= toInt64({floorz}))"),
        ),
        (
            "mv_z_floored".to_owned(),
            format!("if(mv_onfloor = 1, toInt64({floorz}), mv_zstepped)"),
        ),
        (
            "mv_momz_floored".to_owned(),
            format!(
                "toInt32(multiIf(mv_onfloor = 1 AND {momz} < 0, 0, \
                 mv_onfloor = 1, {momz}, \
                 bitAnd({flags}, {MF_NOGRAVITY}) != 0, {momz}, \
                 {momz} = 0, -{}, toInt64({momz}) - {GRAVITY}))",
                GRAVITY * 2
            ),
        ),
        (
            "mv_hitceiling".to_owned(),
            format!("toUInt8(mv_z_floored + toInt64({height}) > toInt64({ceilingz}))"),
        ),
        (
            "mv_z".to_owned(),
            format!(
                "toInt32(if(mv_hitceiling = 1, toInt64({ceilingz}) - toInt64({height}), \
                 mv_z_floored))"
            ),
        ),
        (
            "mv_momz".to_owned(),
            "toInt32(if(mv_hitceiling = 1 AND mv_momz_floored > 0, 0, mv_momz_floored))".to_owned(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn mover() -> Mover<'static> {
        Mover {
            slot: "pl_slot",
            radius: "pl_radius",
            height: "pl_height",
            z: "pl_z",
            flags: "pl_flags",
            is_player: "1",
            momx: "pl_pushx",
            momy: "pl_pushy",
            x: "pl_x",
            y: "pl_y",
            floorz: "pl_floorz",
            ceilingz: "pl_ceilingz",
            subsector: "pl_subsector",
            angle: "pl_angle",
            uses: "pl_uses",
        }
    }

    pub(super) fn world() -> World<'static> {
        World {
            m_x: "w_x",
            m_y: "w_y",
            m_radius: "w_radius",
            m_flags: "w_flags",
            m_linkseq: "w_linkseq",
            alive: "move_at.11",
            floorheight: "w_floor",
            ceilingheight: "w_ceiling",
            line_special: "w_special",
        }
    }

    pub(super) fn pickups() -> Pickups<'static> {
        Pickups {
            m_sprite: "w_sprite",
            m_flags: "w_flags",
            m_z: "w_z",
            skill: "skill",
            start: "pk0",
            alive: "alive0",
        }
    }

    /// Only a look that takes the player as a target reaches the sound
    /// switch, so a thing that looks and sees nothing draws nothing.
    #[test]
    fn the_pass_draws_once_for_each_thing_that_wakes_and_shouts() {
        let bindings = thinkers(&State::default());
        let named = |name: &str| {
            bindings
                .iter()
                .find(|(binding, _)| binding == name)
                .map(|(_, expr)| expr.clone())
                .unwrap_or_else(|| panic!("{name} is bound"))
        };
        let shouts = named("mt_shouts");
        assert!(shouts.contains("l = 1 AND w = 1 AND"), "{shouts}");
        assert_eq!(shouts.matches("a_look_sounds").count(), 1, "{shouts}");
        let index = named("now_prndindex");
        assert!(
            index.starts_with("toUInt8(bitAnd(toUInt32(prev_prndindex) + arraySum(mt_shouts)"),
            "{index}"
        );
        assert!(
            index.contains(&format!("c.{}, cw_chased", enemy::chased::DRAWS)),
            "the chase's own draws are counted too: {index}"
        );
    }

    #[test]
    fn the_loop_runs_as_many_steps_as_the_momentum_needs() {
        let text = steps("mx", "my", "u");
        assert!(text.contains("mx = 0 AND my = 0, 0"));
        assert!(text.contains("> 983040 OR"));
        assert!(text.contains("if(u = 1, 1, 0) +"), "{text}");
        assert!(text.ends_with(", 14, 7))"), "{text}");
    }

    /// A mover fast enough to split gets room for the slide twice over,
    /// because either half of the move can be the one that is blocked.
    #[test]
    fn a_fast_mover_gets_a_step_for_each_half_and_a_slide_for_each() {
        let text = steps(&MAXMOVE.to_string(), "0", "0");
        assert!(text.contains(&format!("> {} OR", MAXMOVE / 2)), "{text}");
        assert!(text.ends_with(&format!(
            ", {}, {}))",
            2 + 2 * SLIDE_BUDGET,
            1 + SLIDE_BUDGET
        )));
    }

    /// `P_XYMovement` halves the move before it tries it, so a blocked half
    /// leaves the other one to spend and the loop comes back for it.
    ///
    /// The demo the live tests run never reaches the speed that splits a
    /// move, so what the two arms below pin is the loop, not the geometry.
    #[test]
    fn a_blocked_half_of_a_split_move_is_still_spent() {
        let sql = xy_movement(&mover(), &world(), &pickups());
        let head = format!(
            "toInt64(multiIf(NOT (move_at.{} = {}), move_at.{}, ",
            moving::PHASE,
            phase::STEP,
            moving::XMOVE
        );
        let halve = format!("bitShiftRight(move_at.{}, 1)", moving::XMOVE);
        let at = sql.find(&head).expect("the loop halves the x move");
        let upto = sql[at..].find(&halve).expect("the loop halves the x move");
        let between = &sql[at + head.len()..at + upto];
        assert!(
            !between.contains(", 0,"),
            "a try the blockmap turned down must not throw the rest away: {between}"
        );
        // Every way out of the slide asks whether a half is left, so the
        // phase the loop falls through to is a bound test and not `DONE`.
        let head = format!(
            "toInt64(multiIf((move_at.{} = {}), {}, ",
            moving::PHASE,
            phase::DONE,
            phase::DONE
        );
        let at = sql.find(&head).expect("the loop decides its next phase");
        let mut depth = 0i32;
        let mut end = 0;
        for (index, letter) in sql[at..].char_indices() {
            match letter {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = index;
                        break;
                    }
                }
                _ => {}
            }
        }
        let arms = &sql[at + head.len()..at + end];
        let fallthrough = arms
            .rsplit(", ")
            .next()
            .expect("multiIf has a last arm")
            .trim_end_matches(')');
        assert_ne!(
            fallthrough,
            phase::DONE.to_string(),
            "a slide that ends must come back for a half the move kept"
        );
    }

    /// `P_UseLines` reads the blockmap the same way `P_SlideMove` does, so
    /// the two share the one traverser the fold holds.
    #[test]
    fn the_loop_traverses_in_one_place() {
        let sql = xy_movement(&mover(), &world(), &pickups());
        assert_eq!(sql.matches("arrayFold((w, s)").count(), 1, "{sql}");
    }

    #[test]
    fn the_loop_is_one_fold() {
        let sql = xy_movement(&mover(), &world(), &pickups());
        assert_eq!(sql.matches("arrayFold((move_at, move_step)").count(), 1);
    }

    #[test]
    fn the_loop_balances_its_parentheses() {
        let sql = xy_movement(&mover(), &world(), &pickups());
        let depth = sql.chars().fold(0i32, |d, c| match c {
            '(' => d + 1,
            ')' => d - 1,
            _ => d,
        });
        assert_eq!(depth, 0);
    }
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// Where each field of a spawn ask sits in its tuple.
pub mod spawning {
    pub const TYPE: usize = 1;
    pub const X: usize = 2;
    pub const Y: usize = 3;
    /// The height to spawn at, or `ONFLOORZ` or `ONCEILINGZ`.
    pub const Z: usize = 4;
    /// How many numbers the tic drew before this spawn's own.
    pub const BASE: usize = 5;
}

/// Where each field of a debris ask sits in its tuple.
///
/// `PTR_ShootTraverse` picks the blood for a thing it can draw blood from
/// and the puff for a wall and for a thing carrying `MF_NOBLOOD`, so which
/// of the two this is comes in with the ask.
pub mod bleeding {
    /// 1 for a blood spot, 0 for a puff.
    pub const BLOOD: usize = 1;
    pub const X: usize = 2;
    pub const Y: usize = 3;
    pub const Z: usize = 4;
    /// What the shot did, which picks a blood spot's own frame.
    pub const DAMAGE: usize = 5;
    /// How far the attack reached. A puff sparks rather than smokes where
    /// that is a punch's reach.
    pub const RANGE: usize = 6;
    /// How many numbers the tic drew before this spawn's own.
    pub const BASE: usize = 7;
}

/// The ClickHouse type of a [`born`] tuple, for a caller that carries a
/// list of them through a fold.
pub const BORN_TYPE: &str = "Tuple(Int32, Int32, Int32, Int32, Int32, Int32, Int32, Int32, \
                             Int32, Int32, Int32, Int32, UInt32)";

/// Where each field of a spawned thing sits in its tuple.
///
/// Everything else a new thing carries is a function of its type and its
/// state, which [`born_column`] reads out of the engine's own tables.
pub mod born {
    pub const X: usize = 1;
    pub const Y: usize = 2;
    pub const Z: usize = 3;
    pub const TYPE: usize = 4;
    pub const STATE: usize = 5;
    pub const TICS: usize = 6;
    pub const FLOORZ: usize = 7;
    pub const CEILINGZ: usize = 8;
    pub const SUBSECTOR: usize = 9;
    pub const LASTLOOK: usize = 10;
    pub const REACTIONTIME: usize = 11;
    pub const MOMZ: usize = 12;
    /// How many numbers the spawn drew.
    pub const DRAWS: usize = 13;
}

/// What a spawn reads: how high each sector stands at this point in the
/// tic, and where the tic's own random index had got to.
pub struct Spawning<'a> {
    pub floorheight: &'a str,
    pub ceilingheight: &'a str,
    pub prndindex: &'a str,
    /// `gameskill`, which decides whether a thing spawns with a reaction
    /// time.
    pub skill: &'a str,
}

/// `P_SpawnMobj` over every ask in `asks`, as a [`born`] tuple each.
///
/// One draw apiece, for `lastlook`. `P_SetThingPosition` puts the thing in
/// the subsector its point falls in, and the floor and the ceiling it
/// stands between are that subsector's sector's.
pub fn spawn_mobj(asks: &str, world: &Spawning<'_>) -> String {
    let a = |field: usize| format!("sp_ask.{field}");
    let values = spawned(
        &a(spawning::TYPE),
        &a(spawning::X),
        &a(spawning::Y),
        &a(spawning::Z),
        &a(spawning::BASE),
        world,
    );
    let body = born_tuple("sp_state", "sp_tics", "toInt32(0)", "toUInt32(1)");
    format!(
        "arrayMap(sp_ask -> {}, {asks})",
        bind::chain_in("spa", &values, &body)
    )
}

/// `P_SpawnPuff` and `P_SpawnBlood` over every ask in `asks`, as a
/// [`born`] tuple each.
///
/// Four draws apiece: two that jitter the height, `P_SpawnMobj`'s own, and
/// one that shortens the wait. `P_SetMobjState` then writes the frame's own
/// wait back over the shortened one wherever the damage or the range moves
/// the thing to another frame.
pub fn spawn_debris(asks: &str, world: &Spawning<'_>) -> String {
    let (values, body) = debris_values(world);
    format!(
        "arrayMap(sp_ask -> {}, {asks})",
        bind::chain_in("spa", &values, &body)
    )
}

/// What one debris ask works out, as the values a body reads and the
/// [`born`] tuple it answers with.
fn debris_values(world: &Spawning<'_>) -> (Vec<(String, String)>, String) {
    let a = |field: usize| format!("sp_ask.{field}");
    let draw = |nth: &str| {
        format!(
            "toInt64(rnd[1 + bitAnd(toUInt32({}) + toUInt32({}) + {nth}, 255)])",
            world.prndindex,
            a(bleeding::BASE),
        )
    };
    let mut values: Vec<(String, String)> = Vec::new();
    values.push((
        "sp_kind".to_owned(),
        format!("toInt32(if({} = 1, mt_blood, mt_puff))", a(bleeding::BLOOD)),
    ));
    // `z += ((P_Random() - P_Random()) << 10)`, in the engine's order.
    values.push((
        "sp_z".to_owned(),
        format!(
            "toInt32(toInt64({}) + bitShiftLeft({} - {}, 10))",
            a(bleeding::Z),
            draw("1"),
            draw("2"),
        ),
    ));
    values.extend(spawned(
        "sp_kind",
        &a(bleeding::X),
        &a(bleeding::Y),
        "sp_z",
        &format!("toUInt32({}) + 2", a(bleeding::BASE)),
        world,
    ));
    // The wait the fourth draw shortens, held at one.
    values.push((
        "sp_short".to_owned(),
        format!("toInt32(greatest(sp_tics - bitAnd({}, 3), 1))", draw("4")),
    ));
    // `P_SetMobjState` writes the frame it moves to and that frame's own
    // wait, so a spot that moves keeps none of the shortened wait.
    values.push((
        "sp_moved".to_owned(),
        format!(
            "toInt32(multiIf({blood} = 0 AND {range} = {MELEERANGE}, s_puff3, \
             {blood} = 0, sp_state, \
             {damage} <= 12 AND {damage} >= 9, s_blood2, \
             {damage} < 9, s_blood3, sp_state))",
            blood = a(bleeding::BLOOD),
            range = a(bleeding::RANGE),
            damage = a(bleeding::DAMAGE),
        ),
    ));
    let body = born_tuple(
        "sp_moved",
        "toInt32(if(sp_moved = sp_state, sp_short, state_tics[1 + sp_moved]))",
        &format!(
            "toInt32(if({} = 1, {}, {}))",
            a(bleeding::BLOOD),
            2 << 16,
            1 << 16
        ),
        "toUInt32(4)",
    );
    (values, body)
}

/// What `P_SpawnMobj` works out for one ask, as the values a body reads.
///
/// `base` is how many numbers the tic drew before this spawn's own, so the
/// draw for `lastlook` is the one after it.
fn spawned(
    kind: &str,
    x: &str,
    y: &str,
    z: &str,
    base: &str,
    world: &Spawning<'_>,
) -> Vec<(String, String)> {
    let info = |table: &str| format!("{table}[1 + sp_type]");
    vec![
        ("sp_type".to_owned(), format!("toInt32({kind})")),
        ("sp_x".to_owned(), format!("toInt32({x})")),
        ("sp_y".to_owned(), format!("toInt32({y})")),
        ("sp_asked_z".to_owned(), format!("toInt32({z})")),
        ("sp_state".to_owned(), info("mobj_spawnstate")),
        (
            "sp_tics".to_owned(),
            "toInt32(state_tics[1 + sp_state])".to_owned(),
        ),
        (
            "sp_lastlook".to_owned(),
            format!(
                "toInt32(rnd[1 + bitAnd(toUInt32({}) + toUInt32({base}) + 1, 255)] % {MAXPLAYERS})",
                world.prndindex
            ),
        ),
        (
            "sp_reactiontime".to_owned(),
            format!(
                "toInt32(if({} != {SK_NIGHTMARE}, {}, 0))",
                world.skill,
                info("mobj_reactiontime")
            ),
        ),
        ("sp_subsector".to_owned(), map::subsector("sp_x", "sp_y")),
        (
            "sp_sector".to_owned(),
            "toInt32(ssec_sector[1 + sp_subsector])".to_owned(),
        ),
        (
            "sp_floorz".to_owned(),
            format!("toInt32({}[1 + sp_sector])", world.floorheight),
        ),
        (
            "sp_ceilingz".to_owned(),
            format!("toInt32({}[1 + sp_sector])", world.ceilingheight),
        ),
        (
            "sp_z_now".to_owned(),
            format!(
                "toInt32(multiIf(sp_asked_z = {ONFLOORZ}, sp_floorz, \
                 sp_asked_z = {ONCEILINGZ}, sp_ceilingz - {}, sp_asked_z))",
                info("mobj_height")
            ),
        ),
    ]
}

/// A [`born`] tuple, from the values [`spawned`] bound.
fn born_tuple(state: &str, tics: &str, momz: &str, draws: &str) -> String {
    format!(
        "(sp_x, sp_y, sp_z_now, sp_type, toInt32({state}), toInt32({tics}), \
         sp_floorz, sp_ceilingz, sp_subsector, sp_lastlook, sp_reactiontime, \
         toInt32({momz}), toUInt32({draws}))"
    )
}

/// The columns a caller hands out rather than reads off a spawn: the
/// identity a thinker takes when it is added, and the order its sector
/// lists it in.
///
/// `P_AddThinker` and `P_SetThingPosition` give each new thing the next of
/// each, so only a caller holding the list can say what they are.
pub const ASSIGNED_COLUMNS: [&str; 2] = ["m_id", "m_linkseq"];

/// What one state column of a newly spawned thing holds, read out of the
/// tables from its [`born`] tuple. `spawn` names one such tuple.
///
/// Every column a spawn leaves at zero answers `toInt32(0)`, because
/// `P_SpawnMobj` clears the whole structure before it fills any of it in.
/// [`ASSIGNED_COLUMNS`] answer `None`, so a caller that walks the columns
/// has to say what it puts in them rather than being handed a zero.
pub fn born_column(column: &str, spawn: &str) -> Option<String> {
    let at = |field: usize| format!("{spawn}.{field}");
    let info = |table: &str| format!("{table}[1 + {}]", at(born::TYPE));
    let state = |table: &str| format!("{table}[1 + {}]", at(born::STATE));
    if ASSIGNED_COLUMNS.contains(&column) {
        return None;
    }
    Some(match column {
        "m_x" => at(born::X),
        "m_y" => at(born::Y),
        "m_z" => at(born::Z),
        "m_type" => at(born::TYPE),
        "m_state" => at(born::STATE),
        "m_tics" => at(born::TICS),
        "m_floorz" => at(born::FLOORZ),
        "m_ceilingz" => at(born::CEILINGZ),
        "m_subsector" => at(born::SUBSECTOR),
        "m_lastlook" => at(born::LASTLOOK),
        "m_reactiontime" => at(born::REACTIONTIME),
        "m_momz" => at(born::MOMZ),
        "m_sprite" => state("state_sprite"),
        "m_frame" => state("state_frame"),
        "m_radius" => info("mobj_radius"),
        "m_height" => info("mobj_height"),
        "m_flags" => info("mobj_flags"),
        "m_health" => info("mobj_spawnhealth"),
        // `P_SpawnMobj` clears the structure, and the contract writes a
        // null player pointer as -1 rather than as 0.
        "m_player" => "toInt8(-1)".to_owned(),
        "m_angle" | "m_target" | "m_tracer" => "toUInt32(0)".to_owned(),
        "m_sp_x" | "m_sp_y" | "m_sp_angle" | "m_sp_type" | "m_sp_options" => {
            "toInt16(0)".to_owned()
        }
        _ => "toInt32(0)".to_owned(),
    })
}

#[cfg(test)]
mod spawn_tests {
    use super::*;
    use crate::tables;

    fn world() -> Spawning<'static> {
        Spawning {
            floorheight: "floorheight",
            ceilingheight: "ceilingheight",
            prndindex: "prndindex",
            skill: "skill",
        }
    }

    /// `mobjinfo`'s own numbers for a name the engine's enum carries.
    fn info(kind: &str, column: &str) -> i64 {
        let types = tables::table("mobjtype").unwrap();
        let at = types
            .texts("name")
            .unwrap()
            .iter()
            .position(|held| *held == kind)
            .expect("the enum carries the name");
        let id = types.ints("id").unwrap()[at];
        tables::table("mobjinfo").unwrap().ints(column).unwrap()[id as usize]
    }

    /// `P_SpawnPuff` and `P_SpawnBlood` hold the shortened wait at one, and
    /// nothing reaches that: the draw takes at most three tics off, and
    /// both frames wait longer than three. The line stays because it is the
    /// engine's; this fails if a frame's own wait ever drops to it.
    #[test]
    fn the_shortened_wait_cannot_run_below_one() {
        let tics = tables::table("states").unwrap().ints("tics").unwrap();
        for kind in ["MT_PUFF", "MT_BLOOD"] {
            let state = info(kind, "spawnstate");
            assert!(
                tics[state as usize] > 3,
                "{kind} spawns in a frame waiting {} tics",
                tics[state as usize]
            );
        }
    }

    /// `P_SetMobjState` runs on into the next frame while the one it
    /// entered waits no tics. Every frame the two chains hold waits, so the
    /// spawn enters one frame and stops, and the generator writes no
    /// cascade. This fails if a frame's own wait ever reaches zero.
    #[test]
    fn no_frame_a_spawn_is_put_into_runs_straight_on() {
        let states = tables::table("states").unwrap();
        let tics = states.ints("tics").unwrap();
        let nextstate = states.ints("nextstate").unwrap();
        for (kind, length) in [("MT_PUFF", 4), ("MT_BLOOD", 3)] {
            let mut state = info(kind, "spawnstate");
            for hop in 0..length {
                assert!(
                    tics[state as usize] > 0,
                    "{kind} frame {hop} waits {} tics",
                    tics[state as usize]
                );
                state = nextstate[state as usize];
            }
            assert_eq!(state, 0, "{kind}: the chain ends where the load says");
        }
    }

    /// A spawn draws once and a puff or a blood spot four times, which is
    /// what every draw after them in the tic sits behind.
    #[test]
    fn a_spawn_draws_once_and_a_debris_four_times() {
        assert!(spawn_mobj("asks", &world()).contains("toUInt32(1))"));
        assert!(spawn_debris("asks", &world()).contains("toUInt32(4))"));
    }

    /// Every column a spawn leaves at zero reads as zero in the column's
    /// own type, because `P_SpawnMobj` clears the structure before it
    /// fills any of it in. The player pointer is the exception: the
    /// contract writes a null one as -1.
    #[test]
    fn a_column_a_spawn_does_not_set_is_zero() {
        for column in ["m_momx", "m_momy", "m_movedir"] {
            assert_eq!(
                born_column(column, "b").as_deref(),
                Some("toInt32(0)"),
                "{column}"
            );
        }
        for column in ["m_angle", "m_target", "m_tracer"] {
            assert_eq!(
                born_column(column, "b").as_deref(),
                Some("toUInt32(0)"),
                "{column}"
            );
        }
        assert_eq!(born_column("m_player", "b").as_deref(), Some("toInt8(-1)"));
        assert_eq!(born_column("m_sp_x", "b").as_deref(), Some("toInt16(0)"));
        assert_eq!(born_column("m_x", "b").as_deref(), Some("b.1"));
    }

    /// The identity a thinker takes and the order its sector lists it in
    /// are the caller's, so a caller walking the columns cannot be handed a
    /// zero for either. A puff and a blood spot carry `MF_NOBLOCKMAP` and
    /// are still in their sector's list, newest first, which is what a
    /// later `P_ChangeSector` and the noise walk read.
    #[test]
    fn the_columns_a_caller_assigns_answer_nothing() {
        for column in ASSIGNED_COLUMNS {
            assert_eq!(born_column(column, "b"), None, "{column}");
        }
        for column in super::super::state_columns() {
            if column.starts_with("m_") && !ASSIGNED_COLUMNS.contains(&column) {
                assert!(born_column(column, "b").is_some(), "{column}");
            }
        }
    }

    /// `z += ((P_Random() - P_Random()) << 10)` leaves the operand order to
    /// the compiler. The pinned ELF saves the first call's answer and
    /// subtracts the second from it, so the earlier draw is the left
    /// operand.
    #[test]
    fn the_height_jitter_subtracts_the_later_draw_from_the_earlier() {
        let (values, _) = debris_values(&world());
        let jitter = values
            .iter()
            .find(|(name, _)| name == "sp_z")
            .map(|(_, expr)| expr.clone())
            .expect("the debris names the height it jitters");
        let offsets: Vec<&str> = jitter
            .match_indices("bitAnd(")
            .filter_map(|(at, _)| jitter[at..].split(", 255)").next())
            .filter_map(|held| held.rsplit("+ ").next())
            .collect();
        assert_eq!(offsets, ["1", "2"], "{jitter}");
    }

    #[test]
    fn every_spawn_expression_balances_its_parentheses() {
        for sql in [spawn_mobj("asks", &world()), spawn_debris("asks", &world())] {
            let depth = sql.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{sql}");
        }
    }
}
