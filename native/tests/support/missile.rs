//! What a monster's missile leaves when it is thrown, read a second time.
//!
//! This is an oracle and nothing else. Native mode throws missiles in SQL;
//! this follows `P_SpawnMissile` and `P_CheckMissileSpawn` in `p_mobj.c`
//! instead, so the two agreeing means something. The map's own answer is
//! not here: whether the half-step lands is what each seeded case is built
//! to decide.

use clickdoom_native::tables;

use super::damage::point_to_angle;
use super::mobj::thing_type;
use super::traverse::fixed_mul;

const FRACUNIT: i64 = 1 << 16;
/// `p_mobj.c`
const MISSILE_HEIGHT: i64 = 4 * 8 * FRACUNIT;
const FUZZ_SHIFT: u32 = 20;
/// `p_mobj.h`
const MF_SOLID: i64 = 2;
const MF_SHOOTABLE: i64 = 4;
const MF_MISSILE: i64 = 0x1_0000;
const MF_SHADOW: i64 = 0x4_0000;
/// `tables.h`
const ANGLE_WRAP: i64 = 1 << 32;
const ANGLETOFINESHIFT: u32 = 19;
const FINEMASK: i64 = 8191;
const QUARTER_TURN: i64 = 2048;

fn column(table: &str, name: &str) -> Vec<i64> {
    tables::table(table)
        .expect("the table is committed")
        .ints(name)
        .expect("the column is an integer")
}

fn wrap32(v: i64) -> i64 {
    i64::from(v as i32)
}

/// One thing, on the fields a missile reads off the two it is thrown
/// between.
#[derive(Clone, Debug)]
pub struct Thing {
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub flags: i64,
}

/// What one `P_SpawnMissile` leaves, in the order the answer names it.
#[derive(Debug, PartialEq, Eq)]
pub struct Thrown {
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub kind: i64,
    pub state: i64,
    pub tics: i64,
    pub momx: i64,
    pub momy: i64,
    pub momz: i64,
    pub angle: i64,
    pub target: i64,
    pub flags: i64,
    pub exploded: bool,
    pub draws: i64,
}

/// The things a missile is thrown between and where the tic's own random
/// index had got to.
pub struct World {
    pub things: Vec<Thing>,
    pub prndindex: i64,
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

    /// `P_SpawnMissile` with `P_CheckMissileSpawn` behind it. `source` and
    /// `dest` are one-based slots. `landed` is what the map said about the
    /// half-step, which each case is built to know.
    pub fn throw(&self, source: i64, dest: i64, kind: i64, base: i64, landed: bool) -> Thrown {
        let from = &self.things[(source - 1) as usize];
        let at = &self.things[(dest - 1) as usize];
        let info = |name: &str| column("mobjinfo", name)[kind as usize];
        let state_tics = column("states", "tics");
        let speed = info("speed");

        let mut draws = 1;
        let mut angle = point_to_angle(at.x - from.x, at.y - from.y);
        let fuzzy = at.flags & MF_SHADOW != 0;
        if fuzzy {
            let fuzz = (self.draw(base, 2) - self.draw(base, 3)) << FUZZ_SHIFT;
            angle = (angle + fuzz).rem_euclid(ANGLE_WRAP);
            draws += 2;
        }
        let fine = angle >> ANGLETOFINESHIFT;
        let momx = wrap32(fixed_mul(speed, self.wave(fine, true)));
        let momy = wrap32(fixed_mul(speed, self.wave(fine, false)));
        let dist = (aprox_distance(at.x - from.x, at.y - from.y) / speed).max(1);
        let momz = wrap32((at.z - from.z) / dist);

        let spawn = info("spawnstate");
        draws += 1;
        let short = (state_tics[spawn as usize] - (self.draw(base, draws) & 3)).max(1);

        let mut thrown = Thrown {
            x: wrap32(from.x + (momx >> 1)),
            y: wrap32(from.y + (momy >> 1)),
            z: wrap32(from.z + MISSILE_HEIGHT + (momz >> 1)),
            kind,
            state: spawn,
            tics: short,
            momx,
            momy,
            momz,
            angle,
            target: source,
            flags: info("flags"),
            exploded: !landed,
            draws,
        };
        if landed {
            return thrown;
        }
        draws += 1;
        let death = info("deathstate");
        thrown.momx = 0;
        thrown.momy = 0;
        thrown.momz = 0;
        thrown.state = death;
        thrown.tics = (state_tics[death as usize] - (self.draw(base, draws) & 3)).max(1);
        thrown.flags = info("flags") & !MF_MISSILE;
        thrown.draws = draws;
        thrown
    }
}

/// `P_AproxDistance`.
fn aprox_distance(dx: i64, dy: i64) -> i64 {
    let (ax, ay) = (wrap32(dx).abs(), wrap32(dy).abs());
    wrap32(ax + ay - (ax.min(ay) >> 1))
}

/// One thing the move test's box reached, on the fields
/// `PIT_CheckThing`'s missile branch reads.
#[derive(Clone, Debug)]
pub struct Reached {
    pub z: i64,
    pub height: i64,
    pub kind: i64,
    pub flags: i64,
}

/// What the missile branch of `PIT_CheckThing` decides for one move.
#[derive(Debug, PartialEq, Eq)]
pub struct Struck {
    /// The place in the list the missile stopped at, 0 for none.
    pub at: usize,
    pub blocked: bool,
    pub damage: i64,
    pub draws: i64,
}

/// The missile as the walk reads it.
pub struct Missile {
    pub z: i64,
    pub height: i64,
    pub kind: i64,
    /// The type of whatever fired it, or -1 where nothing did.
    pub shooter: i64,
}

impl World {
    /// `PIT_CheckThing`'s missile branch over the things the box reached,
    /// in the order the walk reaches them. `shooter_at` is the place in
    /// the list whatever fired the missile stands at, or 0.
    pub fn strike(
        &self,
        it: &Missile,
        touched: &[Reached],
        shooter_at: usize,
        base: i64,
    ) -> Struck {
        let mut answer = Struck {
            at: 0,
            blocked: false,
            damage: 0,
            draws: 0,
        };
        for (at, thing) in touched.iter().enumerate() {
            if it.z > thing.z + thing.height || it.z + it.height < thing.z {
                continue;
            }
            if at + 1 == shooter_at {
                continue;
            }
            let knight = thing_type("MT_KNIGHT");
            let bruiser = thing_type("MT_BRUISER");
            let species = it.shooter != -1
                && (it.shooter == thing.kind
                    || (it.shooter == knight && thing.kind == bruiser)
                    || (it.shooter == bruiser && thing.kind == knight));
            if species && thing.kind != thing_type("MT_PLAYER") {
                answer.at = at + 1;
                answer.blocked = true;
                return answer;
            }
            if thing.flags & MF_SHOOTABLE == 0 {
                if thing.flags & MF_SOLID == 0 {
                    continue;
                }
                answer.at = at + 1;
                answer.blocked = true;
                return answer;
            }
            answer.at = at + 1;
            answer.blocked = true;
            answer.draws = 1;
            answer.damage =
                (self.draw(base, 1) % 8 + 1) * column("mobjinfo", "damage")[it.kind as usize];
            return answer;
        }
        answer
    }

    /// `P_ExplodeMissile`, with `P_XYMovement`'s sky check ahead of it.
    /// `sky` is whether the line the move test named has a sky ceiling
    /// behind it.
    pub fn stop(
        &self,
        kind: i64,
        state: i64,
        tics: i64,
        flags: i64,
        sky: bool,
        base: i64,
    ) -> Stopped {
        let info = |name: &str| column("mobjinfo", name)[kind as usize];
        if sky {
            return Stopped {
                state,
                tics,
                flags,
                removed: true,
                draws: 0,
            };
        }
        let death = info("deathstate");
        Stopped {
            state: death,
            tics: (column("states", "tics")[death as usize] - (self.draw(base, 1) & 3)).max(1),
            flags: flags & !MF_MISSILE,
            removed: false,
            draws: 1,
        }
    }
}

/// What one stop leaves.
#[derive(Debug, PartialEq, Eq)]
pub struct Stopped {
    pub state: i64,
    pub tics: i64,
    pub flags: i64,
    pub removed: bool,
    pub draws: i64,
}
