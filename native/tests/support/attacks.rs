//! What a monster's own attack leaves, read a second time.
//!
//! This is an oracle and nothing else. Native mode attacks in SQL; this
//! follows `A_FaceTarget`, `P_CheckMeleeRange`, `A_TroopAttack` and
//! `A_SargAttack` in `p_enemy.c` instead, so the two agreeing means
//! something.

use clickdoom_native::tables;

use super::damage::point_to_angle;
use super::mobj::thing_type;

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

// ---------------------------------------------------------------------------
// P_RadiusAttack
// ---------------------------------------------------------------------------

/// `p_enemy.c`: what `A_Explode` asks for.
const BOMBDAMAGE: i64 = 128;
/// `p_map.c`: how far the block walk reaches, which the engine's own
/// `(damage + MAXRADIUS) << FRACBITS` leaves at the damage in fixed point
/// because the `MAXRADIUS` term runs off the top of an `int`.
const BLAST_REACH: i64 = BOMBDAMAGE << 16;
/// `p_local.h`
const MAPBLOCKSHIFT: u32 = 23;
/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;

/// The blockmap the walk indexes into.
pub struct Blockmap {
    pub orgx: i64,
    pub orgy: i64,
    pub cols: i64,
    pub rows: i64,
}

/// One damage ask a blast makes, in `inter::hurting`'s order.
#[derive(Debug, PartialEq, Eq)]
pub struct Bomb {
    pub target: usize,
    pub inflictor: usize,
    pub source: usize,
    pub damage: i64,
    pub base: i64,
}

/// One thing as the block walk reads it, beside the fields a damage call
/// reads.
#[derive(Clone, Debug)]
pub struct Standing {
    pub radius: i64,
    pub linkseq: i64,
}

/// The things `PIT_RadiusAttack` would ask `P_CheckSight` about, in the
/// order the block walk reaches them.
///
/// Everything before the sight check is here: the square of cells, the
/// order inside them, the two flags and the distance rule.
pub fn radius_candidates(
    mobjs: &[super::damage::Mobj],
    standing: &[Standing],
    map: &Blockmap,
    spot: usize,
) -> Vec<usize> {
    let at = &mobjs[spot - 1];
    let cell = |slot: usize| {
        let it = &mobjs[slot - 1];
        ((it.y - map.orgy) >> MAPBLOCKSHIFT) * map.cols + ((it.x - map.orgx) >> MAPBLOCKSHIFT)
    };
    // The square of cells, row by row and left to right inside a row.
    let side = |coord: i64, origin: i64, count: i64| {
        let low = ((coord - BLAST_REACH - origin) >> MAPBLOCKSHIFT).max(0);
        let high = ((coord + BLAST_REACH - origin) >> MAPBLOCKSHIFT).min(count - 1);
        low..=high
    };
    let mut cells: Vec<i64> = Vec::new();
    for by in side(at.y, map.orgy, map.rows) {
        for bx in side(at.x, map.orgx, map.cols) {
            cells.push(by * map.cols + bx);
        }
    }

    // The things those cells hold, in the order the walk reaches them.
    let mut reached: Vec<usize> = (1..=mobjs.len())
        .filter(|slot| cells.contains(&cell(*slot)))
        .collect();
    reached.sort_by_key(|slot| {
        (
            cells.iter().position(|c| *c == cell(*slot)),
            -standing[*slot - 1].linkseq,
        )
    });

    // `PIT_RadiusAttack`. The thing the blast goes off at is not named:
    // `P_KillMobj` has already taken `MF_SHOOTABLE` off it.
    let cyborg = thing_type("MT_CYBORG");
    let spider = thing_type("MT_SPIDER");
    reached
        .into_iter()
        .filter(|slot| {
            let it = &mobjs[*slot - 1];
            it.flags & MF_SHOOTABLE != 0
                && it.kind != cyborg
                && it.kind != spider
                && blast_damage(mobjs, standing, spot, *slot) > 0
        })
        .collect()
}

/// What the blast does to one thing: `128` less the wider of the two axes
/// with the thing's own radius taken off, in whole units and held at zero.
/// Nothing at or past `128` is hurt at all.
pub fn blast_damage(
    mobjs: &[super::damage::Mobj],
    standing: &[Standing],
    spot: usize,
    slot: usize,
) -> i64 {
    let at = &mobjs[spot - 1];
    let it = &mobjs[slot - 1];
    let dx = (it.x - at.x).abs();
    let dy = (it.y - at.y).abs();
    let dist = ((dx.max(dy) - standing[slot - 1].radius) >> 16).max(0);
    BOMBDAMAGE - dist
}

/// The world a blast reads, and the reader that says how many numbers one
/// damage call draws.
///
/// The draw count is what puts each ask's base behind the ones before it.
pub struct Blasting<'a> {
    pub mobjs: &'a [super::damage::Mobj],
    pub standing: &'a [Standing],
    pub hurt: &'a super::damage::World,
}

impl Blasting<'_> {
    pub fn asks(
        &self,
        spot: usize,
        source: usize,
        base: i64,
        candidates: &[usize],
        seen: &dyn Fn(usize) -> bool,
    ) -> (Vec<Bomb>, i64) {
        let mut asks: Vec<Bomb> = Vec::new();
        let mut drawn = 0;
        for slot in candidates.iter().copied().filter(|slot| seen(*slot)) {
            let damage = blast_damage(self.mobjs, self.standing, spot, slot);
            asks.push(Bomb {
                target: slot,
                inflictor: spot,
                source,
                damage,
                base: base + drawn,
            });
            drawn += self
                .hurt
                .damage(slot as i64, spot as i64, source as i64, damage, 0)
                .draws;
        }
        (asks, drawn)
    }
}
