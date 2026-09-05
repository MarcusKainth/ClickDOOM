//! What a thing does with its momentum and its states, from `p_mobj.c`.

use super::map::{self, World, answer};
use super::{State, attacks, enemy, inter, maputl, missile, sight};
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
const MF_FLOAT: i64 = 0x4000;
const MF_MISSILE: i64 = 0x1_0000;
const MF_CORPSE: i64 = 0x10_0000;
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
    /// Where the thing faces, which `A_FaceTarget` turns towards the
    /// target and every other entry leaves alone.
    pub const ANGLE: usize = 9;
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
            "a_facetarget".to_owned(),
            format!(
                "assumeNotNull((SELECT id FROM {db}.action_functions \
                 WHERE name = 'A_FaceTarget'))"
            ),
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
        ("mt_troopshot".to_owned(), thing_type(db, "MT_TROOPSHOT")),
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
        "mt_alive",
        format!("arrayMap(v -> toUInt8(1), {})", s("m_x")),
    );
    let standing = World {
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
    for (name, expr) in thing_moves(state, &standing, &slot) {
        bind(&name, expr);
    }
    for (name, expr) in thing_falls(state) {
        bind(&name, expr);
    }
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
    bind(
        "mt_attackers",
        format!(
            "arrayFilter((k, c, n, t) -> c = 1 AND n != 0 AND t != 0 \
             AND (state_action[1 + n] = a_troopattack \
             OR state_action[1 + n] = a_sargattack), \
             mt_slots, mt_cycles, mt_next, {})",
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
             arrayMap(k -> {}, mt_hearers), arrayMap(k -> {}, mt_attackers))",
            pairs("k", &player),
            pairs("k", &target),
            pairs("k", &heard),
            pairs("k", &target),
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
        "mt_attack_seen",
        "arraySlice(mt_seen, 1 + length(mt_lookers) + length(mt_chasers) \
         + length(mt_hearers), length(mt_attackers))"
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
            "arrayMap((k, c, n, w, l, lk, tt, st, tc, th, ll, ty) -> ({}), \
             mt_slots, mt_cycles, mt_next, mt_wakes, mt_looks, mt_looked, mt_target, \
             {}, {}, {}, {}, {})",
            entry_one(&slot, state),
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
    bind("mc_m_angle", read(cycled::ANGLE, "toUInt32"));
    bind("mc_m_state", read(cycled::STATE, "toInt32"));
    bind("mc_m_tics", read(cycled::TICS, "toInt32"));
    bind("mk_m_target", read(cycled::TARGET, "toUInt32"));
    bind("mt_threshold", read(cycled::THRESHOLD, "toInt32"));
    bind("mk_m_lastlook", read(cycled::LASTLOOK, "toInt32"));
    for (column, table) in [("m_sprite", "state_sprite"), ("m_frame", "state_frame")] {
        bind(
            &format!("mc_{column}"),
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
        "tx_shifters",
        "arrayDistinct(arrayConcat(tx_movers, mt_movers))".to_owned(),
    );
    bind("tx_crowded", shifted(state, "tx_shifters"));
    let world = World {
        m_x: "tx_m_x",
        m_y: "tx_m_y",
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
        m_x: "tx_m_x",
        m_y: "tx_m_y",
        m_z: "tz_m_z",
        m_angle: &s("m_angle"),
        m_radius: &s("m_radius"),
        m_height: &s("m_height"),
        m_flags: &s("m_flags"),
        m_type: &s("m_type"),
        m_health: &s("m_health"),
        m_target: "mk_m_target",
        m_movedir: &s("m_movedir"),
        m_movecount: &s("m_movecount"),
        m_reactiontime: &s("m_reactiontime"),
        m_threshold: "mt_threshold",
        m_floorz: "tx_m_floorz",
        m_ceilingz: "tx_m_ceilingz",
        m_subsector: "tx_m_subsector",
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
        ("m_x", enemy::chased::X, "toInt32", "tx_m_x".to_owned()),
        ("m_y", enemy::chased::Y, "toInt32", "tx_m_y".to_owned()),
        ("m_z", enemy::chased::Z, "toInt32", "tz_m_z".to_owned()),
        (
            "m_angle",
            enemy::chased::ANGLE,
            "toUInt32",
            "mc_m_angle".to_owned(),
        ),
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
        (
            "m_floorz",
            enemy::chased::FLOORZ,
            "toInt32",
            "tx_m_floorz".to_owned(),
        ),
        (
            "m_ceilingz",
            enemy::chased::CEILINGZ,
            "toInt32",
            "tx_m_ceilingz".to_owned(),
        ),
        (
            "m_subsector",
            enemy::chased::SUBSECTOR,
            "toInt32",
            "tx_m_subsector".to_owned(),
        ),
    ];
    let mut standing: Vec<String> = vec![String::new(); enemy::chased::FLAGS];
    for (_, member, cast, array) in &held {
        standing[member - 1] = format!("{cast}({array}[k])");
    }
    standing[enemy::chased::DRAWS - 1] = "toUInt32(0)".to_owned();
    standing[enemy::chased::STUCK - 1] = "toUInt8(0)".to_owned();
    // A thing no chase reaches attacks nothing and keeps its flags.
    standing[enemy::chased::STATE - 1] = "toInt32(-1)".to_owned();
    standing[enemy::chased::FLAGS - 1] = format!("toInt32({}[k])", s("m_flags"));
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
    bind(
        "cw_attack",
        format!(
            "arrayMap(c -> toInt32(c.{}), cw_slot)",
            enemy::chased::STATE
        ),
    );
    bind("cq_m_flags", {
        let held = s("m_flags");
        format!(
            "arrayMap((k, c) -> toInt32(if(indexOf(mt_movers, k) = 0, {held}[k], c.{})), \
             mt_slots, cw_slot)",
            enemy::chased::FLAGS
        )
    });
    bind(
        "mk_m_state",
        "arrayMap((k, a) -> toInt32(if(a = -1, mc_m_state[k], a)), mt_slots, cw_attack)".to_owned(),
    );
    bind(
        "mk_m_tics",
        "arrayMap((k, a) -> toInt32(if(a = -1, mc_m_tics[k], state_tics[1 + a])), \
         mt_slots, cw_attack)"
            .to_owned(),
    );
    for (column, table) in [("m_sprite", "state_sprite"), ("m_frame", "state_frame")] {
        bind(
            &format!("mk_{column}"),
            format!(
                "arrayMap((k, a) -> toInt32(if(a = -1, mc_{column}[k], {table}[1 + a])), \
                 mt_slots, cw_attack)"
            ),
        );
    }
    for (column, member, cast, _) in &held {
        let name = if *column == "m_angle" {
            "cq_m_angle".to_owned()
        } else {
            format!("mk_{column}")
        };
        bind(&name, format!("arrayMap(c -> {cast}(c.{member}), cw_slot)"));
    }
    // The attack reads what the state cycle and the chase left, and what
    // it leaves stands over them.
    for (name, expr) in strikes(state, &world) {
        bind(&name, expr);
    }
    bind(
        "mk_m_angle",
        format!(
            "arrayMap((k, v) -> toUInt32(if(k = at_one AND at_one != 0, at_struck.{}, v)), \
             mt_slots, cq_m_angle)",
            attacks::attacked::ANGLE
        ),
    );
    bind(
        "mk_m_flags",
        format!(
            "arrayMap((k, v) -> toInt32(if(k = at_one AND at_one != 0, at_struck.{}, v)), \
             mt_slots, cq_m_flags)",
            attacks::attacked::FLAGS
        ),
    );
    bind(
        "now_prndindex",
        format!(
            "toUInt8(bitAnd(toUInt32({}) + arraySum(mt_shouts) \
             + arraySum(arrayMap(c -> c.{}, cw_chased)) + at_draws, 255))",
            s("prndindex"),
            enemy::chased::DRAWS
        ),
    );
    for (name, expr) in removed(state, &slot) {
        bind(&name, expr);
    }
    bind(
        "now_unresolved",
        format!(
            "toUInt8({} = 1 OR arrayExists(a -> a.{} = 1, mt_two) \
             OR arrayExists(c -> c.{} = 1, cw_chased) OR cw_crowded = 1 \
             OR tx_crowded = 1 OR tx_unrun = 1 OR tx_crossed = 1 \
             OR tz_unrun = 1 OR at_unrun = 1)",
            s("unresolved"),
            cycled::STUCK,
            enemy::chased::STUCK
        ),
    );
    bindings
}

/// `P_RemoveMobj` for the things whose state cycle reached `S_NULL`.
///
/// `P_SetMobjState` removes a thing rather than entering state zero, and
/// `P_RemoveThinker` takes it off the list at the end of the tic. The
/// arrays close the gap the thing leaves, so every slot above it moves
/// down by one and every pointer naming one of those moves with it. A
/// pointer at the thing that was taken becomes 0, which is what the
/// contract says none means.
///
/// A fireball the tic threw goes on the end of the list, behind what the
/// compaction left, and takes one of each counter.
fn removed(state: &State, player: &str) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let mut bindings: Vec<(String, String)> = Vec::new();
    let mut bind = |name: &str, expr: String| bindings.push((name.to_owned(), expr));

    bind(
        "mt_gone",
        format!(
            "arrayMap((k, c, n) -> toUInt8(k != {player} AND c = 1 AND n = 0), \
             mt_slots, mt_cycles, mt_next)"
        ),
    );
    bind(
        "mt_kept",
        "arrayMap(g -> toUInt8(1 - g), mt_gone)".to_owned(),
    );
    // Where each slot ends up, or 0 for the one that was taken.
    bind(
        "mt_slot",
        "arrayMap((a, c) -> toUInt32(if(a = 1, c, 0)), mt_kept, arrayCumSum(mt_kept))".to_owned(),
    );

    let moved_slot = |slot: &str| format!("toUInt32(if({slot} = 0, 0, mt_slot[{slot}]))");
    for column in super::state_columns() {
        // `m_id` is the slot itself, which is read off the compacted list.
        if !column.starts_with("m_") || column == "m_id" {
            continue;
        }
        let held = if THINKER_COLUMNS.contains(&column) {
            format!("mk_{column}")
        } else {
            s(column)
        };
        // What the claw left, before the renumbering, because the target
        // the damage sets is a slot like any other.
        let held = match clawed(column) {
            Some(member) => format!(
                "arrayMap((k, v) -> toInt32(if(at_clawed = 1 AND k = at_target, \
                 mt_hurt.{member}, v)), mt_slots, {held})"
            ),
            None => held,
        };
        let held = if POINTERS.contains(&column) {
            format!("arrayMap(t -> {}, {held})", moved_slot("t"))
        } else {
            held
        };
        // `P_AddThinker` puts a new thing on the end of the list, so a
        // fireball the tic threw goes behind what survived the compaction.
        let born = match missile::born_column(column, "t") {
            Some(value) => {
                let value = if POINTERS.contains(&column) {
                    moved_slot(&value)
                } else {
                    value
                };
                format!("arrayMap(t -> {value}, mt_thrown)")
            }
            None => format!(
                "arrayMap((t, i) -> toUInt32({} + i - 1), mt_thrown, arrayEnumerate(mt_thrown))",
                s("next_linkseq")
            ),
        };
        bind(
            &format!("now_{column}"),
            format!("arrayConcat(arrayFilter((v, a) -> a = 1, {held}, mt_kept), {born})"),
        );
    }
    bind(
        "now_m_id",
        "arrayMap(n -> toUInt32(n), arrayEnumerate(now_m_x))".to_owned(),
    );
    bind(
        "now_sec_soundtarget",
        format!(
            "arrayMap(t -> {}, {})",
            moved_slot("t"),
            s("sec_soundtarget")
        ),
    );
    bind("now_p_attacker", moved_slot(&s("p_attacker")));
    bind("now_p_mo", format!("toUInt32(mt_slot[{player}])"));
    // Every thing the tic threw took one of each counter.
    for column in ["next_seq", "next_linkseq"] {
        bind(
            &format!("now_{column}"),
            format!("toUInt32({} + length(mt_thrown))", s(column)),
        );
    }
    bindings
}

/// Where a column the claw moves sits in the answer `P_DamageMobj` gives.
fn clawed(column: &str) -> Option<usize> {
    Some(match column {
        "m_health" => inter::hurt::HEALTH,
        "m_flags" => inter::hurt::FLAGS,
        "m_state" => inter::hurt::STATE,
        "m_tics" => inter::hurt::TICS,
        "m_momx" => inter::hurt::MOMX,
        "m_momy" => inter::hurt::MOMY,
        "m_momz" => inter::hurt::MOMZ,
        "m_height" => inter::hurt::HEIGHT,
        "m_reactiontime" => inter::hurt::REACTIONTIME,
        "m_target" => inter::hurt::TARGET,
        "m_threshold" => inter::hurt::THRESHOLD,
        _ => return None,
    })
}

/// The mobj array columns the thinker writes, which the compaction reads
/// from it rather than from the tic's own start.
const THINKER_COLUMNS: [&str; 21] = [
    "m_flags",
    "m_state",
    "m_tics",
    "m_target",
    "m_lastlook",
    "m_sprite",
    "m_frame",
    "m_x",
    "m_y",
    "m_z",
    "m_angle",
    "m_movedir",
    "m_movecount",
    "m_reactiontime",
    "m_threshold",
    "m_floorz",
    "m_ceilingz",
    "m_subsector",
    "m_momx",
    "m_momy",
    "m_momz",
];

/// The mobj array columns that hold a slot rather than a value of their
/// own. `sec_soundtarget`, `p_attacker` and `p_mo` hold one too and are
/// written beside them.
const POINTERS: [&str; 2] = ["m_target", "m_tracer"];

/// `A_TroopAttack` and `A_SargAttack` for the things whose state cycle
/// reached one, and the damage a claw that lands does.
///
/// The routine turns the thing towards its target, takes it off ambush and
/// either claws the target or, for an imp out of reach, throws a fireball.
/// Both the routine and `P_DamageMobj` are folded over their ask lists
/// rather than mapped, so a tic that reaches neither runs neither body.
///
/// Three cases say the tic could not be produced: more than one thing
/// reaching a routine, because the second would draw from an index the
/// first moves; the fireball, which wants a missile spawned; and a claw
/// that kills, which owes the kill count and whatever the corpse drops.
fn strikes(state: &State, map: &World<'_>) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let mut bindings: Vec<(String, String)> = Vec::new();
    let mut bind = |name: &str, expr: String| bindings.push((name.to_owned(), expr));

    // One ask per attacker: the slot, the routine its frame carries, the
    // sight `P_CheckMeleeRange` needs, and how many numbers the tic drew
    // before it.
    bind(
        "at_asks",
        format!(
            "arrayMap(k -> (toUInt32(k), toInt32(state_action[1 + mt_next[k]]), \
             toUInt8(mt_attack_seen[indexOf(mt_attackers, k)]), \
             toUInt32(arraySum(arraySlice(mt_shouts, 1, k)) \
             + arraySum(arrayMap(c -> toUInt32(c.{}), arraySlice(cw_slot, 1, k))))), \
             mt_attackers)",
            enemy::chased::DRAWS
        ),
    );
    let world = attacks::Attacking {
        m_x: &s("m_x"),
        m_y: &s("m_y"),
        m_angle: "cq_m_angle",
        m_flags: "cq_m_flags",
        m_type: &s("m_type"),
        m_target: "mk_m_target",
        prndindex: &s("prndindex"),
    };
    bind("at_struck", attacks::attack_fold("at_asks", &world));
    // The one attacker a tic carries and what it drew before its own call.
    // A tic reaching more than one is refused below and reads neither.
    bind(
        "at_one",
        "toUInt32(if(length(mt_attackers) = 1, mt_attackers[1], 0))".to_owned(),
    );
    bind(
        "at_base",
        format!(
            "toUInt32(arraySum(arrayMap(a -> toUInt32(a.{}), at_asks)))",
            attacks::striking::BASE
        ),
    );

    // The thinker stage's `P_DamageMobj` ask list, and the one call over
    // it. A missile's impact, a barrel's blast and a monster's hitscan all
    // hurt things in this stage; each joins this list rather than standing
    // up a call of its own, because the routine is among the largest
    // things the statement carries and every copy costs a tic that hurts
    // nothing.
    bind(
        "mt_hurt_asks",
        format!(
            "arraySlice([{}], 1, at_struck.{})",
            attacks::claw_ask("at_struck", "greatest(at_one, 1)", "mk_m_target", "at_base"),
            attacks::attacked::CLAWED,
        ),
    );
    // The target as the stage has left it so far. `m_health` and
    // `m_height` have no writer ahead of this one, so they stand as the
    // tic started.
    let hurting = inter::Hurting {
        m_x: "mk_m_x",
        m_y: "mk_m_y",
        m_z: "mk_m_z",
        m_momx: "mk_m_momx",
        m_momy: "mk_m_momy",
        m_momz: "mk_m_momz",
        m_reactiontime: "mk_m_reactiontime",
        m_type: &s("m_type"),
        m_state: "mk_m_state",
        m_tics: "mk_m_tics",
        m_flags: "cq_m_flags",
        m_health: &s("m_health"),
        m_height: &s("m_height"),
        m_target: "mk_m_target",
        m_threshold: "mk_m_threshold",
        m_player: &s("m_player"),
        prndindex: &s("prndindex"),
        readyweapon: &s("p_readyweapon"),
    };
    bind("mt_hurt", inter::damage_fold("mt_hurt_asks", &hurting));

    // `P_SpawnMissile` for an imp whose claw did not reach. The fireball
    // is the only missile a routine throws here, so its type is the one
    // constant.
    bind(
        "mt_throw_asks",
        format!(
            "arraySlice([(at_one, toUInt32(mk_m_target[greatest(at_one, 1)]), \
             mt_troopshot, at_base + toUInt32(at_struck.{}))], 1, at_struck.{})",
            attacks::attacked::DRAWS,
            attacks::attacked::THROWS,
        ),
    );
    let throwing = missile::Throwing {
        m_x: "mk_m_x",
        m_y: "mk_m_y",
        m_z: "mk_m_z",
        m_radius: &s("m_radius"),
        m_height: &s("m_height"),
        m_flags: "cq_m_flags",
        prndindex: &s("prndindex"),
    };
    let spawning = Spawning {
        floorheight: &s("sec_floorheight"),
        ceilingheight: &s("sec_ceilingheight"),
        prndindex: &s("prndindex"),
        skill: "skill",
    };
    bind(
        "mt_thrown",
        missile::spawn_fold("mt_throw_asks", &throwing, &spawning, map),
    );

    bind(
        "at_clawed",
        format!("toUInt8(at_struck.{})", attacks::attacked::CLAWED),
    );
    bind(
        "at_target",
        "toUInt32(if(at_clawed = 1, mk_m_target[greatest(at_one, 1)], 0))".to_owned(),
    );
    bind(
        "at_draws",
        format!(
            "toUInt32(toUInt32(at_struck.{}) + toUInt32(mt_hurt.{}) \
             + arraySum(arrayMap(t -> toUInt32(t.{}), mt_thrown)))",
            attacks::attacked::DRAWS,
            inter::hurt::DRAWS,
            missile::thrown::DRAWS,
        ),
    );
    bind(
        "at_unrun",
        format!(
            "toUInt8(length(mt_attackers) > 1 OR at_struck.{stuck} = 1 \
             OR mt_hurt.{counted} = 1 OR mt_hurt.{drop} != -1 \
             OR arrayExists(t -> t.{thrown} = 1, mt_thrown))",
            stuck = attacks::attacked::STUCK,
            counted = inter::hurt::COUNTED,
            drop = inter::hurt::DROP,
            thrown = missile::thrown::STUCK,
        ),
    );
    bindings
}

/// The first state a cycle enters, and `A_Look` where the state carries
/// it.
fn entry_one(slot: &str, state: &State) -> String {
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
            "toUInt8(multiIf(k = {slot}, 0, c = 0, 0, n = 0, 0, \
             state_action[1 + n] != 0 AND state_action[1 + n] != a_look \
             AND state_action[1 + n] != a_chase \
             AND state_action[1 + n] != a_facetarget \
             AND state_action[1 + n] != a_troopattack \
             AND state_action[1 + n] != a_sargattack, 1, 0))"
        ),
        format!("toUInt8({enters})"),
        format!(
            "toUInt32(if({enters} AND state_action[1 + n] = a_facetarget AND tt != 0, \
             {}, {}[k]))",
            facing(state, "tt", "k"),
            state.get("m_angle"),
        ),
    ];
    members.join(", ")
}

/// `R_PointToAngle2` from a thing to what it faces, which is the whole of
/// what `A_FaceTarget` leaves behind for a target it can see plainly.
fn facing(state: &State, target: &str, slot: &str) -> String {
    let at = |column: &str| format!("{}[{slot}]", state.get(column));
    let of = |column: &str| format!("{}[{target}]", state.get(column));
    crate::sql::fixed::point_to_angle(
        &format!("toInt64({}) - toInt64({})", of("m_x"), at("m_x")),
        &format!("toInt64({}) - toInt64({})", of("m_y"), at("m_y")),
        "tantoangle",
    )
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
             AND state_action[1 + {entering}] != a_chase \
             AND state_action[1 + {entering}] != a_facetarget) \
             OR state_tics[1 + {entering}] = 0)))",
            held(cycled::STUCK)
        ),
        format!("toUInt8({enters} OR a.{} = 1)", cycled::MOVED),
        held(cycled::ANGLE),
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

/// `P_XYMovement` for the things that are not the player.
///
/// A thing that is not the player has no slide, no use line and no
/// pickup: a move `P_TryMove` refuses stops it dead. The engine clamps the
/// momentum to `MAXMOVE` and then spends it in at most two parts, because
/// halving once puts both axes under half of `MAXMOVE`, so the two parts
/// are written out rather than looped. Each part asks `P_TryMove` once for
/// every thing the part covers, so a tic moving many things walks twice.
///
/// Everything between the two walks is one value per thing that moves, and
/// a tic that moves nothing folds over empty lists. Only the columns the
/// rest of the tic reads are put back over every slot, where a slot
/// nothing moved keeps what it came in with; the player's own columns,
/// which `P_PlayerThink` has already written, pass through that way.
pub fn thing_moves(state: &State, world: &World<'_>, player: &str) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let mut bindings: Vec<(String, String)> = Vec::new();
    let mut bind = |name: &str, expr: String| bindings.push((name.to_owned(), expr));
    // Read at the slot a mover holds, inside a lambda taking `k`.
    let at = |column: &str| format!("{}[k]", s(column));
    // Read at a mover's own place in the lists below, inside one taking `i`.
    let half = MAXMOVE / 2;
    let quarter = FRACUNIT / 4;
    // One value per mover, in the order `tx_movers` names them.
    let over_movers = |expr: &str| format!("arrayMap(k -> {expr}, tx_movers)");
    let by_place = |expr: &str| format!("arrayMap(i -> {expr}, arrayEnumerate(tx_movers))");

    bind(
        "tx_moving",
        format!(
            "arrayMap((k, mx, my) -> toUInt8(k != {player} AND (mx != 0 OR my != 0)), \
             mt_slots, {}, {})",
            s("m_momx"),
            s("m_momy"),
        ),
    );
    bind(
        "tx_movers",
        "arrayFilter((k, m) -> m = 1, mt_slots, tx_moving)".to_owned(),
    );
    bind(
        "tx_at",
        "arrayMap(k -> indexOf(tx_movers, k), mt_slots)".to_owned(),
    );
    for (name, column) in [
        ("x", "m_x"),
        ("y", "m_y"),
        ("z", "m_z"),
        ("flags", "m_flags"),
        ("floorz", "m_floorz"),
        ("ceilingz", "m_ceilingz"),
        ("subsector", "m_subsector"),
    ] {
        bind(&format!("tx_hold_{name}"), over_movers(&at(column)));
    }
    for axis in ["x", "y"] {
        bind(
            &format!("tx_mom{axis}"),
            over_movers(&clamp(&at(&format!("m_mom{axis}")))),
        );
    }
    // The loop halves while either axis is over half of `MAXMOVE`, and the
    // clamp above means one halving is enough. `P_TryMove` is asked for the
    // half C division leaves, and the half the shift leaves is what
    // remains; the two differ by one for a negative odd momentum.
    bind(
        "tx_splits",
        by_place(&format!(
            "toUInt8(tx_momx[i] > {half} OR tx_momy[i] > {half})"
        )),
    );
    for axis in ["x", "y"] {
        bind(
            &format!("tx_first{axis}"),
            by_place(&format!(
                "toInt32(if(tx_splits[i] = 1, intDiv(tx_mom{axis}[i], 2), tx_mom{axis}[i]))"
            )),
        );
        bind(
            &format!("tx_left{axis}"),
            by_place(&format!(
                "toInt32(if(tx_splits[i] = 1, bitShiftRight(tx_mom{axis}[i], 1), 0))"
            )),
        );
    }

    // The first part. A thing carries itself out of the way, so both walks
    // read the world as the tic left it.
    let asking = |x: String, y: String| {
        map::asking(
            "tx_movers[i]",
            &x,
            &y,
            &format!("{}[tx_movers[i]]", s("m_radius")),
            &format!("{}[tx_movers[i]]", s("m_height")),
            "tx_hold_z[i]",
            "tx_hold_flags[i]",
            "0",
        )
    };
    bind(
        "tx_asks_one",
        by_place(&asking(
            "toInt64(tx_hold_x[i]) + toInt64(tx_firstx[i])".to_owned(),
            "toInt64(tx_hold_y[i]) + toInt64(tx_firsty[i])".to_owned(),
        )),
    );
    bind("tx_one", map::try_moves("tx_asks_one", world));
    bind(
        "tx_ok_one",
        by_place(&format!("toUInt8(tx_one[i].{} = 1)", answer::OK)),
    );
    for axis in ["x", "y"] {
        bind(
            &format!("tx_{axis}_one"),
            by_place(&format!(
                "toInt32(if(tx_ok_one[i] = 1, \
                 toInt64(tx_hold_{axis}[i]) + toInt64(tx_first{axis}[i]), \
                 toInt64(tx_hold_{axis}[i])))"
            )),
        );
    }

    // The second part, for the moves the halving split. A thing the first
    // part stopped still tries it, because the engine zeroes the momentum
    // and keeps what is left of the move.
    bind(
        "tx_split_at",
        "arrayFilter(i -> tx_splits[i] = 1, arrayEnumerate(tx_movers))".to_owned(),
    );
    bind(
        "tx_asks_two",
        format!(
            "arrayMap(i -> {}, tx_split_at)",
            asking(
                "toInt64(tx_x_one[i]) + toInt64(tx_leftx[i])".to_owned(),
                "toInt64(tx_y_one[i]) + toInt64(tx_lefty[i])".to_owned(),
            )
        ),
    );
    bind("tx_two", map::try_moves("tx_asks_two", world));
    bind("tx_two_at", by_place("indexOf(tx_split_at, i)"));
    bind(
        "tx_ok_two",
        by_place(&format!(
            "toUInt8(tx_two_at[i] != 0 AND tx_two[greatest(tx_two_at[i], 1)].{} = 1)",
            answer::OK
        )),
    );
    for axis in ["x", "y"] {
        bind(
            &format!("tx_{axis}"),
            by_place(&format!(
                "toInt32(if(tx_ok_two[i] = 1, \
                 toInt64(tx_{axis}_one[i]) + toInt64(tx_left{axis}[i]), toInt64(tx_{axis}_one[i])))"
            )),
        );
    }

    // A move nothing allowed leaves the thing with nothing left to spend.
    bind(
        "tx_stopped",
        by_place("toUInt8(tx_ok_one[i] = 0 OR (tx_splits[i] = 1 AND tx_ok_two[i] = 0))"),
    );
    for axis in ["x", "y"] {
        bind(
            &format!("tx_spent{axis}"),
            by_place(&format!(
                "toInt32(if(tx_stopped[i] = 1, 0, tx_mom{axis}[i]))"
            )),
        );
    }
    // What the thing stands between, from the last part `P_TryMove`
    // allowed.
    for (name, field) in [
        ("floorz", answer::FLOORZ),
        ("ceilingz", answer::CEILINGZ),
        ("subsector", answer::SUBSECTOR),
    ] {
        bind(
            &format!("tx_{name}"),
            by_place(&format!(
                "toInt32(multiIf(tx_ok_two[i] = 1, tx_two[greatest(tx_two_at[i], 1)].{field}, \
                 tx_ok_one[i] = 1, tx_one[i].{field}, tx_hold_{name}[i]))"
            )),
        );
    }

    // The friction `P_XYMovement` ends with. A thing above its floor keeps
    // what it has, and so does a corpse with speed left that is half off a
    // step, which is a floor its own subsector does not give it.
    bind(
        "tx_airborne",
        by_place("toUInt8(toInt64(tx_hold_z[i]) > toInt64(tx_floorz[i]))"),
    );
    bind(
        "tx_sliding",
        format!(
            "arrayMap(i -> toUInt8(bitAnd(tx_hold_flags[i], {MF_CORPSE}) != 0 \
             AND (tx_spentx[i] > {quarter} OR tx_spentx[i] < -{quarter} \
             OR tx_spenty[i] > {quarter} OR tx_spenty[i] < -{quarter}) \
             AND tx_floorz[i] != {floor}[1 + ssec_sector[1 + tx_subsector[i]]]), \
             arrayEnumerate(tx_movers))",
            floor = s("sec_floorheight"),
        ),
    );
    bind(
        "tx_stops",
        by_place(&format!(
            "toUInt8(tx_spentx[i] > -{STOPSPEED} AND tx_spentx[i] < {STOPSPEED} \
             AND tx_spenty[i] > -{STOPSPEED} AND tx_spenty[i] < {STOPSPEED})"
        )),
    );
    for axis in ["x", "y"] {
        bind(
            &format!("tx_mom{axis}_left"),
            by_place(&format!(
                "toInt32(multiIf(tx_airborne[i] = 1 OR tx_sliding[i] = 1, tx_spent{axis}[i], \
                 tx_stops[i] = 1, 0, bitShiftRight(toInt64(tx_spent{axis}[i]) * {FRICTION}, 16)))"
            )),
        );
    }

    // What the rest of the tic reads, over every slot.
    for (column, moved) in [
        ("m_x", "tx_x"),
        ("m_y", "tx_y"),
        ("m_floorz", "tx_floorz"),
        ("m_ceilingz", "tx_ceilingz"),
        ("m_subsector", "tx_subsector"),
    ] {
        let held = s(column);
        bind(
            &format!("tx_{column}"),
            format!(
                "arrayMap((k, i) -> toInt32(if(i = 0, {held}[k], {moved}[i])), mt_slots, tx_at)"
            ),
        );
    }
    for axis in ["x", "y"] {
        let held = s(&format!("m_mom{axis}"));
        bind(
            &format!("mk_m_mom{axis}"),
            format!(
                "arrayMap((k, i) -> toInt32(if(i = 0, {held}[k], tx_mom{axis}_left[i])), \
                 mt_slots, tx_at)"
            ),
        );
    }

    // `P_TryMove` runs `P_CrossSpecialLine` for the special lines a move
    // that landed crossed, which a thrust can push a monster over. A move
    // the walk refused crosses nothing, so only a part that landed counts.
    bind(
        "tx_special",
        by_place(&format!(
            "toUInt8((tx_ok_one[i] = 1 AND notEmpty(tx_one[i].{spechit})) \
             OR (tx_ok_two[i] = 1 \
             AND notEmpty(tx_two[greatest(tx_two_at[i], 1)].{spechit})))",
            spechit = answer::SPECHIT,
        )),
    );
    bind(
        "tx_crossed",
        "toUInt8(arrayExists(v -> v = 1, tx_special))".to_owned(),
    );

    // Neither a missile nor a skull in flight is a thing this moves. A
    // move a missile cannot make ends it, and one a skull cannot make
    // slams it back into its spawn frames, where this would take friction
    // off both.
    bind(
        "tx_unrun",
        format!(
            "toUInt8(arrayExists(k -> bitAnd({}, {}) != 0, tx_movers))",
            at("m_flags"),
            MF_MISSILE | MF_SKULLFLY,
        ),
    );
    bindings
}

/// Whether two of the things moving this tic stand close enough that one's
/// move changes what the other is told.
///
/// The reach is the momentum a thing spends this tic or the step a chase
/// takes, whichever it is doing, so one test covers both lists.
fn shifted(state: &State, movers: &str) -> String {
    let s = |column: &str| state.get(column);
    let reach = |slot: &str| {
        format!(
            "toInt64({r}[{slot}]) + bitShiftLeft(toInt64(mobj_speed[1 + {t}[{slot}]]), 16) \
             + greatest(abs(toInt64({mx}[{slot}])), abs(toInt64({my}[{slot}])))",
            r = s("m_radius"),
            t = s("m_type"),
            mx = s("m_momx"),
            my = s("m_momy"),
        )
    };
    let axis = |array: &str| {
        format!(
            "abs(toInt64({array}[a]) - toInt64({array}[b])) < {} + {}",
            reach("a"),
            reach("b"),
        )
    };
    format!(
        "toUInt8(arrayExists((a, i) -> arrayExists(b -> {} AND {}, \
         arraySlice({movers}, i + 1)), {movers}, arrayEnumerate({movers})))",
        axis(&s("m_x")),
        axis(&s("m_y")),
    )
}

/// `P_ZMovement` for the things that are not the player.
///
/// `P_MobjThinker` runs it on a thing standing off its floor or carrying
/// height, which after the shots is the blood and the puffs they leave.
/// The height moves, the floor and the ceiling clip it, and gravity pulls
/// on whatever the floor is not already holding.
///
/// The routine asks the map nothing, so this is arithmetic over the
/// things that fall and nothing else. The player's own copy carries the
/// view height a step up lowers, which no other thing has.
pub fn thing_falls(state: &State) -> Vec<(String, String)> {
    let s = |column: &str| state.get(column);
    let mut bindings: Vec<(String, String)> = Vec::new();
    let mut bind = |name: &str, expr: String| bindings.push((name.to_owned(), expr));
    let at = |column: &str| format!("{}[k]", s(column));
    let over_fallers = |expr: &str| format!("arrayMap(k -> {expr}, tz_fallers)");
    let by_place = |expr: &str| format!("arrayMap(i -> {expr}, arrayEnumerate(tz_fallers))");

    bind(
        "tz_falling",
        format!(
            "arrayMap((k, z, fz, mz) -> toUInt8(k != {} AND (z != fz OR mz != 0)), \
             mt_slots, {}, tx_m_floorz, {})",
            s("p_mo"),
            s("m_z"),
            s("m_momz"),
        ),
    );
    bind(
        "tz_fallers",
        "arrayFilter((k, f) -> f = 1, mt_slots, tz_falling)".to_owned(),
    );
    bind(
        "tz_at",
        "arrayMap(k -> indexOf(tz_fallers, k), mt_slots)".to_owned(),
    );
    for (name, column) in [
        ("z", "m_z"),
        ("momz", "m_momz"),
        ("height", "m_height"),
        ("flags", "m_flags"),
    ] {
        bind(&format!("tz_hold_{name}"), over_fallers(&at(column)));
    }
    for (name, array) in [("floorz", "tx_m_floorz"), ("ceilingz", "tx_m_ceilingz")] {
        bind(
            &format!("tz_hold_{name}"),
            over_fallers(&format!("{array}[k]")),
        );
    }

    bind(
        "tz_stepped",
        by_place("toInt64(tz_hold_z[i]) + toInt64(tz_hold_momz[i])"),
    );
    bind(
        "tz_onfloor",
        by_place("toUInt8(tz_stepped[i] <= toInt64(tz_hold_floorz[i]))"),
    );
    bind(
        "tz_floored",
        by_place("if(tz_onfloor[i] = 1, toInt64(tz_hold_floorz[i]), tz_stepped[i])"),
    );
    // The floor takes what is falling onto it, and gravity pulls on what
    // the floor is not holding. A thing that has just started falling
    // takes two pulls, which is what makes the first tic of a drop move
    // twice as far as gravity alone.
    bind(
        "tz_pulled",
        by_place(&format!(
            "toInt32(multiIf(tz_onfloor[i] = 1 AND tz_hold_momz[i] < 0, 0, \
             tz_onfloor[i] = 1, tz_hold_momz[i], \
             bitAnd(tz_hold_flags[i], {MF_NOGRAVITY}) != 0, tz_hold_momz[i], \
             tz_hold_momz[i] = 0, -{}, toInt64(tz_hold_momz[i]) - {GRAVITY}))",
            GRAVITY * 2
        )),
    );
    bind(
        "tz_hitceiling",
        by_place(
            "toUInt8(tz_floored[i] + toInt64(tz_hold_height[i]) > toInt64(tz_hold_ceilingz[i]))",
        ),
    );
    bind(
        "tz_z",
        by_place(
            "toInt32(if(tz_hitceiling[i] = 1, \
             toInt64(tz_hold_ceilingz[i]) - toInt64(tz_hold_height[i]), tz_floored[i]))",
        ),
    );
    bind(
        "tz_momz",
        by_place("toInt32(if(tz_hitceiling[i] = 1 AND tz_pulled[i] > 0, 0, tz_pulled[i]))"),
    );

    // `A_Chase` writes the height column itself, so the fall hands it the
    // height rather than writing it, the way the move hands it the place.
    for (column, moved) in [("m_z", "tz_z"), ("m_momz", "tz_momz")] {
        let held = s(column);
        bind(
            &format!("tz_{column}"),
            format!(
                "arrayMap((k, i) -> toInt32(if(i = 0, {held}[k], {moved}[i])), mt_slots, tz_at)"
            ),
        );
    }
    bind("mk_m_momz", "tz_m_momz".to_owned());

    // What `P_ZMovement` does that this does not: a skull in flight bounces
    // off what it reaches, a floating thing rises and sinks towards its
    // target, and a missile that reaches the floor or the ceiling goes off.
    bind(
        "tz_unrun",
        format!(
            "toUInt8(arrayExists(k -> bitAnd({flags}, {}) != 0 \
             OR (bitAnd({flags}, {MF_FLOAT}) != 0 AND {} != 0), tz_fallers))",
            MF_SKULLFLY | MF_MISSILE,
            at("m_target"),
            flags = at("m_flags"),
        ),
    );
    bindings
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
