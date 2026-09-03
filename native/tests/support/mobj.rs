//! What a spawn leaves behind, read a second time.
//!
//! This is an oracle and nothing else. Native mode spawns in SQL; this
//! follows `p_mobj.c` instead, so the two agreeing means something.

use clickdoom_native::tables;

use super::spawn::{Node, point_in_subsector};

const FRACUNIT: i64 = 1 << 16;
/// `d_player.h`
const MAXPLAYERS: i64 = 4;
/// `doomdef.h`
const SK_NIGHTMARE: i64 = 4;
/// `p_local.h`
const MELEERANGE: i64 = 64 * FRACUNIT;
/// `p_mobj.h`
const ONFLOORZ: i64 = i32::MIN as i64;
const ONCEILINGZ: i64 = i32::MAX as i64;

/// One newly spawned thing, on the fields `P_SpawnMobj` fills in.
#[derive(Debug, PartialEq, Eq)]
pub struct Born {
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub kind: i64,
    pub state: i64,
    pub tics: i64,
    pub floorz: i64,
    pub ceilingz: i64,
    pub subsector: i64,
    pub lastlook: i64,
    pub reactiontime: i64,
    pub momz: i64,
    pub draws: i64,
}

/// One `P_SpawnPuff` or `P_SpawnBlood` call.
#[derive(Clone, Copy)]
pub struct Debris {
    pub blood: bool,
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub damage: i64,
    pub range: i64,
    /// How many numbers the tic drew before this spawn's own.
    pub base: i64,
}

/// The level and the engine tables a spawn reads.
pub struct World {
    pub nodes: Vec<Node>,
    pub ssec_sector: Vec<usize>,
    pub floorheight: Vec<i64>,
    pub ceilingheight: Vec<i64>,
    pub skill: i64,
}

fn column(table: &str, name: &str) -> Vec<i64> {
    tables::table(table)
        .expect("the table is committed")
        .ints(name)
        .expect("the column is an integer")
}

/// The `mobjtype` id the engine's own enum gives `name`.
pub fn thing_type(name: &str) -> i64 {
    let types = tables::table("mobjtype").expect("mobjtype is committed");
    let ids = types.ints("id").expect("id is an integer");
    let names = types.texts("name").expect("name is a string");
    let at = names
        .iter()
        .position(|held| *held == name)
        .expect("the engine's enum carries the name");
    ids[at]
}

fn wrap32(v: i64) -> i64 {
    i64::from(v as i32)
}

impl World {
    /// `P_Random`, `nth` calls after the index stood at `prnd`.
    fn draw(&self, prnd: i64, nth: i64) -> i64 {
        column("rndtable", "value")[((prnd + nth) & 255) as usize]
    }

    /// `P_SpawnMobj`, drawing once for `lastlook`.
    pub fn spawn(&self, prnd: i64, kind: i64, x: i64, y: i64, z: i64, base: i64) -> Born {
        let at = kind as usize;
        let state = column("mobjinfo", "spawnstate")[at];
        let subsector = i64::from(point_in_subsector(x as i32, y as i32, &self.nodes));
        let sector = self.ssec_sector[subsector as usize];
        let (floorz, ceilingz) = (self.floorheight[sector], self.ceilingheight[sector]);
        let height = column("mobjinfo", "height")[at];
        Born {
            x,
            y,
            z: match z {
                ONFLOORZ => floorz,
                ONCEILINGZ => wrap32(ceilingz - height),
                _ => z,
            },
            kind,
            state,
            tics: column("states", "tics")[state as usize],
            floorz,
            ceilingz,
            subsector,
            lastlook: self.draw(prnd, base + 1) % MAXPLAYERS,
            reactiontime: if self.skill != SK_NIGHTMARE {
                column("mobjinfo", "reactiontime")[at]
            } else {
                0
            },
            momz: 0,
            draws: 1,
        }
    }

    /// `P_SpawnPuff` and `P_SpawnBlood`, drawing four times.
    pub fn debris(&self, prnd: i64, ask: &Debris) -> Born {
        let kind = thing_type(if ask.blood { "MT_BLOOD" } else { "MT_PUFF" });
        let jitter = (self.draw(prnd, ask.base + 1) - self.draw(prnd, ask.base + 2)) << 10;
        let z = wrap32(ask.z + jitter);
        let mut born = self.spawn(prnd, kind, ask.x, ask.y, z, ask.base + 2);
        born.momz = if ask.blood { 2 * FRACUNIT } else { FRACUNIT };
        born.draws = 4;
        born.tics = (born.tics - (self.draw(prnd, ask.base + 4) & 3)).max(1);
        // `P_SetMobjState` writes the frame's own wait over the shortened
        // one wherever it moves the thing.
        let along = |hops: usize| {
            let mut state = column("mobjinfo", "spawnstate")[kind as usize];
            for _ in 0..hops {
                state = column("states", "nextstate")[state as usize];
            }
            state
        };
        let moved = if !ask.blood {
            if ask.range == MELEERANGE {
                Some(along(2))
            } else {
                None
            }
        } else if (9..=12).contains(&ask.damage) {
            Some(along(1))
        } else if ask.damage < 9 {
            Some(along(2))
        } else {
            None
        };
        if let Some(state) = moved {
            born.state = state;
            born.tics = column("states", "tics")[state as usize];
        }
        born
    }
}
