//! What a monster's own attack leaves, read a second time.
//!
//! This is an oracle and nothing else. Native mode attacks in SQL; this
//! follows `A_FaceTarget`, `P_CheckMeleeRange`, `A_TroopAttack` and
//! `A_SargAttack` in `p_enemy.c` instead, so the two agreeing means
//! something.

use clickdoom_native::tables;

use super::damage::point_to_angle;

const FRACUNIT: i64 = 1 << 16;
/// `p_local.h`
const MELEERANGE: i64 = 64 * FRACUNIT;
const MELEE_SLOP: i64 = 20 * FRACUNIT;
/// `p_enemy.c`
const FACE_SHIFT: u32 = 21;
/// `p_mobj.h`
const MF_AMBUSH: i64 = 32;
const MF_SHADOW: i64 = 0x4_0000;
/// `tables.h`
const ANGLE_WRAP: i64 = 1 << 32;

fn column(table: &str, name: &str) -> Vec<i64> {
    tables::table(table)
        .expect("the table is committed")
        .ints(name)
        .expect("the column is an integer")
}

fn wrap32(v: i64) -> i64 {
    i64::from(v as i32)
}

/// One thing, on the fields an attack reads.
#[derive(Clone, Debug)]
pub struct Fighter {
    pub x: i64,
    pub y: i64,
    pub angle: i64,
    pub kind: i64,
    pub flags: i64,
    /// The slot this one is chasing, 0 for none.
    pub target: usize,
}

/// What one attack leaves.
#[derive(Debug, PartialEq, Eq)]
pub struct Attacked {
    pub angle: i64,
    pub flags: i64,
    pub clawed: bool,
    pub damage: i64,
    pub throws: bool,
    pub draws: i64,
}

/// Which of the two routines the frame carries.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Routine {
    Troop,
    Sarg,
}

/// The things an attack runs between and where the tic's own random index
/// had got to.
pub struct World {
    pub fighters: Vec<Fighter>,
    pub prndindex: i64,
}

impl World {
    fn draw(&self, base: i64, nth: i64) -> i64 {
        column("rndtable", "value")[((self.prndindex + base + nth) & 255) as usize]
    }

    /// `A_TroopAttack` or `A_SargAttack` for the thing in `slot`. `sees` is
    /// what `P_CheckSight` answered for it and its target.
    pub fn attack(&self, slot: usize, routine: Routine, sees: bool, base: i64) -> Attacked {
        let it = &self.fighters[slot - 1];
        let mut answer = Attacked {
            angle: it.angle,
            flags: it.flags,
            clawed: false,
            damage: 0,
            throws: false,
            draws: 0,
        };
        if it.target == 0 {
            return answer;
        }
        let at = &self.fighters[it.target - 1];

        // `A_FaceTarget`.
        answer.flags = it.flags & !MF_AMBUSH;
        let mut angle = point_to_angle(at.x - it.x, at.y - it.y);
        if at.flags & MF_SHADOW != 0 {
            angle += (self.draw(base, 1) - self.draw(base, 2)) << FACE_SHIFT;
            answer.draws += 2;
        }
        answer.angle = angle.rem_euclid(ANGLE_WRAP);

        // `P_CheckMeleeRange`.
        let radius = column("mobjinfo", "radius")[at.kind as usize];
        let near = aprox_distance(at.x - it.x, at.y - it.y) < MELEERANGE - MELEE_SLOP + radius;
        if !(near && sees) {
            answer.throws = routine == Routine::Troop;
            return answer;
        }
        answer.clawed = true;
        answer.draws += 1;
        let roll = self.draw(base, answer.draws);
        answer.damage = match routine {
            Routine::Troop => (roll % 8 + 1) * 3,
            Routine::Sarg => (roll % 10 + 1) * 4,
        };
        answer
    }
}

/// `P_AproxDistance`.
fn aprox_distance(dx: i64, dy: i64) -> i64 {
    let (ax, ay) = (wrap32(dx).abs(), wrap32(dy).abs());
    wrap32(ax + ay - (ax.min(ay) >> 1))
}
