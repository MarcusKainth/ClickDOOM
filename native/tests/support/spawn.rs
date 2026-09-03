//! What `P_SetupLevel` spawns, read from the WAD a second time.
//!
//! This is an oracle and nothing else. Native mode spawns the level in SQL
//! from the decoded tables; this reads the map lumps straight out of the
//! WAD and follows `p_mobj.c`, `p_spec.c` and `r_main.c` instead, so the
//! two agreeing means something.

use clickdoom_native::tables;
use clickdoom_native::wad::Wad;
use clickdoom_spec::native_state::sector_thinker_kind::{FIRE_FLICKER, GLOW, LIGHT_FLASH, STROBE};

const FRACBITS: u32 = 16;
const ANG45: u32 = 0x2000_0000;
const MF_COUNTKILL: i32 = 0x40_0000;
const MF_COUNTITEM: i32 = 0x80_0000;
const MF_AMBUSH: i32 = 32;
const MTF_AMBUSH: i16 = 8;
const NF_SUBSECTOR: u32 = 0x8000;
const INITIAL_HEALTH: i32 = 100;
const FLASH_MINTIME: i32 = 7;
const FLASH_MAXTIME: i32 = 64;
const STROBEBRIGHT: i32 = 5;
const FASTDARK: i32 = 15;
const SLOWDARK: i32 = 35;
const FLICKER_COUNT: i32 = 4;

/// One spawned mobj, in thinker order.
#[derive(Debug, PartialEq, Eq)]
pub struct Mobj {
    pub x: i32,
    pub y: i32,
    pub angle: u32,
    pub kind: i32,
    pub tics: i32,
    pub state: i32,
    pub health: i32,
    pub radius: i32,
    pub height: i32,
    pub flags: i32,
    pub reactiontime: i32,
    pub lastlook: i32,
    pub player: i8,
    pub subsector: i32,
    pub spawnpoint: [i16; 5],
}

/// One light thinker, in sector order. `minlight` is not here: it needs
/// the sector's line list, which the SQL takes from `P_GroupLines`'s own
/// output rather than from the lump.
#[derive(Debug, PartialEq, Eq)]
pub struct Thinker {
    pub sector: i32,
    pub kind: u8,
    pub count: i32,
    pub mintime: i32,
    pub maxtime: i32,
    pub direction: i32,
}

/// The level the setup leaves behind.
pub struct Level {
    pub mobjs: Vec<Mobj>,
    pub thinkers: Vec<Thinker>,
    /// Every `P_Random` call the setup makes, things then light thinkers.
    pub draws: u32,
    pub totalkills: i32,
    pub totalitems: i32,
    pub totalsecret: i32,
}

fn i16_at(bytes: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

/// `R_PointOnSide`, from `r_main.c`.
fn point_on_side(x: i32, y: i32, node: &Node) -> usize {
    if node.dx == 0 {
        return usize::from(if x <= node.x {
            node.dy > 0
        } else {
            node.dy < 0
        });
    }
    if node.dy == 0 {
        return usize::from(if y <= node.y {
            node.dx < 0
        } else {
            node.dx > 0
        });
    }
    let dx = x.wrapping_sub(node.x);
    let dy = y.wrapping_sub(node.y);
    if (node.dy ^ node.dx ^ dx ^ dy) & i32::MIN != 0 {
        return usize::from((node.dy ^ dx) & i32::MIN != 0);
    }
    let left = fixed_mul(node.dy >> FRACBITS, dx);
    let right = fixed_mul(dy, node.dx >> FRACBITS);
    usize::from(right >= left)
}

fn fixed_mul(a: i32, b: i32) -> i32 {
    ((i64::from(a) * i64::from(b)) >> FRACBITS) as i32
}

pub struct Node {
    x: i32,
    y: i32,
    dx: i32,
    dy: i32,
    children: [u16; 2],
}

/// `R_PointInSubsector`, from `r_main.c`.
pub fn point_in_subsector(x: i32, y: i32, nodes: &[Node]) -> i32 {
    if nodes.is_empty() {
        return 0;
    }
    let mut at = (nodes.len() - 1) as u32;
    while at & NF_SUBSECTOR == 0 {
        let node = &nodes[at as usize];
        at = u32::from(node.children[point_on_side(x, y, node)]);
    }
    (at & !NF_SUBSECTOR) as i32
}

pub fn nodes(wad: &Wad<'_>, map: &str) -> Vec<Node> {
    let bytes = wad.map_lump(map, "NODES").expect("the map has nodes").bytes;
    bytes
        .as_chunks::<28>()
        .0
        .iter()
        .map(|n| Node {
            x: i32::from(i16_at(n, 0)) << FRACBITS,
            y: i32::from(i16_at(n, 2)) << FRACBITS,
            dx: i32::from(i16_at(n, 4)) << FRACBITS,
            dy: i32::from(i16_at(n, 6)) << FRACBITS,
            children: [u16_at(n, 24), u16_at(n, 26)],
        })
        .collect()
}

/// `P_LoadThings` and `P_SpawnSpecials` at `skill`, single player.
pub fn level(wad: &Wad<'_>, map: &str, skill: i32) -> Level {
    let mobjinfo = tables::table("mobjinfo").unwrap();
    let doomednum = mobjinfo.ints("doomednum").unwrap();
    let field = |name: &str| mobjinfo.ints(name).unwrap();
    let (spawnstate, spawnhealth) = (field("spawnstate"), field("spawnhealth"));
    let (reactiontime, radius, height) = (field("reactiontime"), field("radius"), field("height"));
    let flags = field("flags");
    let state_tics = tables::table("states").unwrap().ints("tics").unwrap();
    let rnd = tables::table("rndtable").unwrap().ints("value").unwrap();
    let nodes = nodes(wad, map);

    let bit = match skill {
        0 => 1,
        4 => 4,
        _ => 1 << (skill - 1),
    };
    let mut draws: u32 = 0;
    // `P_Random`, reading the engine's own table.
    let draw = |draws: &mut u32| {
        *draws += 1;
        rnd[(*draws & 0xff) as usize] as i32
    };

    let mut level = Level {
        mobjs: Vec::new(),
        thinkers: Vec::new(),
        draws: 0,
        totalkills: 0,
        totalitems: 0,
        totalsecret: 0,
    };
    let things = wad
        .map_lump(map, "THINGS")
        .expect("the map has things")
        .bytes;
    for t in things.as_chunks::<10>().0 {
        let spawnpoint = [
            i16_at(t, 0),
            i16_at(t, 2),
            i16_at(t, 4),
            i16_at(t, 6),
            i16_at(t, 8),
        ];
        let (kind, options) = (spawnpoint[3], spawnpoint[4]);
        // P_SpawnMapThing, in its own order.
        let player = kind == 1;
        if kind == 11 || kind <= 0 || (kind <= 4 && !player) {
            continue;
        }
        if !player && (options & 16 != 0 || options & bit == 0) {
            continue;
        }
        let at = if player {
            0
        } else {
            doomednum
                .iter()
                .position(|d| *d == i64::from(kind))
                .expect("mobjinfo carries the thing's type")
        };
        let state = spawnstate[at] as i32;
        let mut mobj = Mobj {
            x: i32::from(spawnpoint[0]) << FRACBITS,
            y: i32::from(spawnpoint[1]) << FRACBITS,
            angle: ANG45.wrapping_mul((i32::from(spawnpoint[2]) / 45) as u32),
            kind: at as i32,
            tics: state_tics[state as usize] as i32,
            state,
            health: if player {
                INITIAL_HEALTH
            } else {
                spawnhealth[at] as i32
            },
            radius: radius[at] as i32,
            height: height[at] as i32,
            flags: flags[at] as i32,
            reactiontime: reactiontime[at] as i32,
            lastlook: draw(&mut draws) % 4,
            player: if player { 0 } else { -1 },
            subsector: 0,
            spawnpoint: if player { [0; 5] } else { spawnpoint },
        };
        mobj.subsector = point_in_subsector(mobj.x, mobj.y, &nodes);
        if !player {
            if mobj.tics > 0 {
                mobj.tics = 1 + draw(&mut draws) % mobj.tics;
            }
            if mobj.flags & MF_COUNTKILL != 0 {
                level.totalkills += 1;
            }
            if mobj.flags & MF_COUNTITEM != 0 {
                level.totalitems += 1;
            }
            if options & MTF_AMBUSH != 0 {
                mobj.flags |= MF_AMBUSH;
            }
        }
        level.mobjs.push(mobj);
    }

    // P_SpawnSpecials, sector by sector. A flash and a free-running
    // strobe each draw their count; a synchronised strobe starts at one.
    let sectors = wad
        .map_lump(map, "SECTORS")
        .expect("the map has sectors")
        .bytes;
    for (at, s) in sectors.as_chunks::<26>().0.iter().enumerate() {
        let special = i16_at(s, 22);
        if special == 9 {
            level.totalsecret += 1;
        }
        let (kind, mintime, maxtime) = match special {
            1 => (LIGHT_FLASH, FLASH_MINTIME, FLASH_MAXTIME),
            2 | 4 | 13 => (STROBE, FASTDARK, STROBEBRIGHT),
            3 | 12 => (STROBE, SLOWDARK, STROBEBRIGHT),
            8 => (GLOW, 0, 0),
            17 => (FIRE_FLICKER, 0, 0),
            _ => continue,
        };
        let count = match special {
            1 => (draw(&mut draws) & FLASH_MAXTIME) + 1,
            2..=4 => (draw(&mut draws) & 7) + 1,
            12 | 13 => 1,
            17 => FLICKER_COUNT,
            _ => 0,
        };
        level.thinkers.push(Thinker {
            sector: at as i32,
            kind,
            count,
            mintime,
            maxtime,
            direction: if kind == GLOW { -1 } else { 0 },
        });
    }
    level.draws = draws;
    level
}
