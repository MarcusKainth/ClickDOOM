//! What a hit does to what it reaches, read a second time.
//!
//! This is an oracle and nothing else. Native mode damages in SQL; this
//! follows `p_inter.c` instead, so the two agreeing means something.

use clickdoom_native::tables;

use super::mobj::thing_type;
use super::traverse::fixed_mul;

const FRACUNIT: i64 = 1 << 16;
/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
const MF_JUSTHIT: i64 = 64;
const MF_NOGRAVITY: i64 = 512;
const MF_DROPOFF: i64 = 0x400;
const MF_NOCLIP: i64 = 0x1000;
const MF_FLOAT: i64 = 0x4000;
const MF_CORPSE: i64 = 0x10_0000;
const MF_COUNTKILL: i64 = 0x40_0000;
const MF_SKULLFLY: i64 = 0x100_0000;
/// `p_local.h`
const BASETHRESHOLD: i64 = 100;
/// `doomdef.h`
const WP_CHAINSAW: i64 = 7;
/// `tables.h`
const ANG180: i64 = 0x8000_0000;
const ANGLE_WRAP: i64 = 1 << 32;
const ANGLETOFINESHIFT: u32 = 19;
const FINEMASK: i64 = 8191;
const QUARTER_TURN: i64 = 2048;
/// `p_inter.c`
const FALL_HEIGHT: i64 = 64 * FRACUNIT;
const FALL_DAMAGE: i64 = 40;

/// One thing, on the fields a hit reads and writes.
#[derive(Clone, Debug)]
pub struct Mobj {
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub momx: i64,
    pub momy: i64,
    pub momz: i64,
    pub kind: i64,
    pub state: i64,
    pub tics: i64,
    pub flags: i64,
    pub health: i64,
    pub height: i64,
    pub target: i64,
    pub threshold: i64,
    /// The player number, or -1 for a thing nobody plays.
    pub player: i64,
}

/// What one `P_DamageMobj` call leaves.
#[derive(Debug, PartialEq, Eq)]
pub struct Hurt {
    pub health: i64,
    pub flags: i64,
    pub state: i64,
    pub tics: i64,
    pub momx: i64,
    pub momy: i64,
    pub momz: i64,
    pub height: i64,
    pub reactiontime: i64,
    pub target: i64,
    pub threshold: i64,
    pub killed: bool,
    pub counted: bool,
    pub drop: i64,
    pub draws: i64,
    pub stuck: bool,
}

fn column(table: &str, name: &str) -> Vec<i64> {
    tables::table(table)
        .expect("the table is committed")
        .ints(name)
        .expect("the column is an integer")
}

/// The `action_functions` id the engine's own table gives a routine.
fn named(name: &str) -> i64 {
    let actions = tables::table("action_functions").expect("the table is committed");
    let at = actions
        .texts("name")
        .expect("name is a string")
        .iter()
        .position(|held| *held == name)
        .expect("the engine carries the routine");
    actions.ints("id").expect("id is an integer")[at]
}

fn wrap32(v: i64) -> i64 {
    i64::from(v as i32)
}

/// `R_PointToAngle2`, over the engine's own arctangent table.
pub fn point_to_angle(dx: i64, dy: i64) -> i64 {
    let tantoangle = column("tantoangle", "value");
    let slope_div = |num: i64, den: i64| {
        let (num, den) = (num as u32 as u64, den as u32 as u64);
        if den < 512 {
            return 2048;
        }
        ((num.wrapping_mul(8) as u32 as u64) / (den >> 8)).min(2048) as usize
    };
    let (dx, dy) = (wrap32(dx), wrap32(dy));
    if dx == 0 && dy == 0 {
        return 0;
    }
    let (ax, ay) = (
        i64::from((dx as i32).unsigned_abs()),
        i64::from((dy as i32).unsigned_abs()),
    );
    let yx = tantoangle[slope_div(ay, ax)];
    let xy = tantoangle[slope_div(ax, ay)];
    let angle = if dx >= 0 && dy >= 0 {
        if dx > dy { yx } else { 1073741823 - xy }
    } else if dx >= 0 {
        if ax > ay {
            ANGLE_WRAP - yx
        } else {
            3221225472 + xy
        }
    } else if dy >= 0 {
        if ax > ay {
            2147483647 - yx
        } else {
            1073741824 + xy
        }
    } else if ax > ay {
        2147483648 + yx
    } else {
        3221225471 - xy
    };
    angle & (ANGLE_WRAP - 1)
}

/// The engine tables and the state a call reads around the two things.
pub struct World {
    pub mobjs: Vec<Mobj>,
    pub prndindex: i64,
    /// The weapon in the player's hands.
    pub readyweapon: i64,
}

impl World {
    fn draw(&self, base: i64, nth: i64) -> i64 {
        column("rndtable", "value")[((self.prndindex + base + nth) & 255) as usize]
    }

    fn wave(&self, fine: i64, quarter: bool) -> i64 {
        let finesine = column("finesine", "value");
        let at = if quarter { fine + QUARTER_TURN } else { fine };
        finesine[(at & FINEMASK) as usize]
    }

    /// `P_DamageMobj`, with `P_KillMobj` where the target dies. `target`,
    /// `inflictor` and `source` are one-based slots, and 0 is none.
    pub fn damage(&self, target: i64, inflictor: i64, source: i64, damage: i64, base: i64) -> Hurt {
        let it = &self.mobjs[(target - 1) as usize];
        let at = it.kind as usize;
        let info = |name: &str| column("mobjinfo", name)[at];
        let state_tics = column("states", "tics");
        let lands = it.flags & MF_SHOOTABLE != 0 && it.health > 0;
        let mut hurt = Hurt {
            health: it.health,
            flags: it.flags,
            state: it.state,
            tics: it.tics,
            momx: it.momx,
            momy: it.momy,
            momz: it.momz,
            height: it.height,
            reactiontime: 0,
            target: it.target,
            threshold: it.threshold,
            killed: false,
            counted: false,
            drop: -1,
            draws: 0,
            stuck: false,
        };
        if !lands {
            return hurt;
        }
        hurt.stuck = it.player != -1;
        let flying = it.flags & MF_SKULLFLY != 0;
        if flying {
            hurt.momx = 0;
            hurt.momy = 0;
            hurt.momz = 0;
        }
        let hit = (inflictor != 0).then(|| &self.mobjs[(inflictor - 1) as usize]);
        let credited = (source != 0).then(|| &self.mobjs[(source - 1) as usize]);
        let pushes = hit.is_some()
            && it.flags & MF_NOCLIP == 0
            && credited.is_none_or(|from| from.player == -1 || self.readyweapon != WP_CHAINSAW);
        let mut draws = 1;
        if pushes {
            let from = hit.expect("a push has an inflictor");
            let mut angle = point_to_angle(it.x - from.x, it.y - from.y);
            let mut thrust = wrap32(damage * (FRACUNIT >> 3) * 100 / info("mass"));
            let may_fall =
                damage < FALL_DAMAGE && damage > it.health && it.z - from.z > FALL_HEIGHT;
            if may_fall {
                draws += 1;
                if self.draw(base, 1) & 1 != 0 {
                    angle = (angle + ANG180) & (ANGLE_WRAP - 1);
                    thrust *= 4;
                }
            }
            let fine = angle >> ANGLETOFINESHIFT;
            hurt.momx = wrap32(hurt.momx + fixed_mul(thrust, self.wave(fine, true)));
            hurt.momy = wrap32(hurt.momy + fixed_mul(thrust, self.wave(fine, false)));
        }
        hurt.draws = draws;
        let second = self.draw(base, draws);
        hurt.health = it.health - damage;
        if hurt.health <= 0 {
            hurt.killed = true;
            hurt.counted = it.flags & MF_COUNTKILL != 0;
            let mut flags = it.flags & !(MF_SHOOTABLE | MF_FLOAT | MF_SKULLFLY);
            if it.kind != thing_type("MT_SKULL") {
                flags &= !MF_NOGRAVITY;
            }
            hurt.flags = flags | MF_CORPSE | MF_DROPOFF;
            hurt.height >>= 2;
            let state = if hurt.health < -info("spawnhealth") && info("xdeathstate") != 0 {
                info("xdeathstate")
            } else {
                info("deathstate")
            };
            hurt.state = state;
            hurt.tics = (state_tics[state as usize] - (second & 3)).max(1);
            hurt.stuck |= self.routine_is_unwritten(it.state, state);
            hurt.drop = match it.kind {
                k if k == thing_type("MT_POSSESSED") || k == thing_type("MT_WOLFSS") => {
                    thing_type("MT_CLIP")
                }
                k if k == thing_type("MT_SHOTGUY") => thing_type("MT_SHOTGUN"),
                k if k == thing_type("MT_CHAINGUY") => thing_type("MT_CHAINGUN"),
                _ => -1,
            };
            return hurt;
        }
        if second < info("painchance") && !flying {
            hurt.flags |= MF_JUSTHIT;
            hurt.state = info("painstate");
            hurt.tics = state_tics[hurt.state as usize];
        }
        hurt.reactiontime = 0;
        let vile = thing_type("MT_VILE");
        let chases = (it.threshold == 0 || it.kind == vile)
            && source != 0
            && source != target
            && credited.is_some_and(|from| from.kind != vile);
        if chases {
            hurt.target = source;
            hurt.threshold = BASETHRESHOLD;
            // `P_SetMobjState` has already moved the thing where the pain
            // frame was entered, and the engine reads the frame it is in
            // now.
            if hurt.state == info("spawnstate") && info("seestate") != 0 {
                hurt.state = info("seestate");
                hurt.tics = state_tics[hurt.state as usize];
            }
        }
        hurt.stuck |= self.routine_is_unwritten(it.state, hurt.state);
        hurt
    }

    /// Whether the frame the call entered carries a routine the SQL does
    /// not run. `A_Pain` and `A_Scream` only make a noise.
    fn routine_is_unwritten(&self, held: i64, entered: i64) -> bool {
        if entered == held {
            return false;
        }
        let action = column("states", "action")[entered as usize];
        action != 0 && action != named("A_Pain") && action != named("A_Scream")
    }
}
