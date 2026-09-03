//! The hitscan attacks read a second time.
//!
//! This is an oracle and nothing else. Native mode aims and shoots in SQL;
//! this follows `p_map.c` and `p_pspr.c` instead, so the two agreeing
//! means something.

#[cfg(feature = "clickhouse-tests")]
use super::db::Fixture;
use super::traverse::{Map, Thing, fixed_div, fixed_mul, nudge};
#[cfg(feature = "clickhouse-tests")]
use clickhouse::Row;
#[cfg(feature = "clickhouse-tests")]
use serde::Deserialize;

const FRACBITS: u32 = 16;
const FRACUNIT: i64 = 1 << FRACBITS;
/// `p_map.c`: how far above and below itself a thing looks.
const TOPSLOPE: i64 = 100 * FRACUNIT / 160;
/// `doomdata.h`
const ML_TWOSIDED: i64 = 4;
/// `p_mobj.h`
const MF_SHOOTABLE: i64 = 4;
/// `tables.h`
const ANGLETOFINESHIFT: u32 = 19;
const FINEMASK: i64 = 8191;
const QUARTER_TURN: i64 = 2048;
/// `p_pspr.c`
const AIMRANGE: i64 = 16 * 64 * FRACUNIT;
const AIMSWING: u32 = 1 << 26;

/// What the aim reads on top of the blockmap: the level's heights and the
/// mobjs' own extents.
pub struct Level {
    pub map: Map,
    /// The first 8,192 entries of the engine's `finesine`.
    pub finesine: Vec<i64>,
    pub line_flags: Vec<i64>,
    pub line_front: Vec<i64>,
    pub line_back: Vec<i64>,
    pub line_side1: Vec<i64>,
    pub line_special: Vec<i64>,
    pub ceilingpic: Vec<i64>,
    pub skyflatnum: i64,
    pub floorheight: Vec<i64>,
    pub ceilingheight: Vec<i64>,
    /// One per mobj slot, in the same order as `map.things`.
    pub m_z: Vec<i64>,
    pub m_height: Vec<i64>,
    pub m_flags: Vec<i64>,
}

/// One aim, as `P_AimLineAttack` is called.
#[derive(Clone, Copy)]
pub struct Ask {
    /// The one-based mobj slot doing the shooting.
    pub shooter: i64,
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub height: i64,
    pub angle: u32,
    pub range: i64,
    /// The slope a shot leaves at. An aim works its own out and ignores
    /// this.
    pub slope: i64,
}

/// What a shot reached, as `PTR_ShootTraverse` left it.
#[derive(Debug, PartialEq, Eq)]
pub struct Shot {
    /// 0 nothing to spawn on, 1 a line, 2 a thing.
    pub kind: u8,
    pub id: i64,
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub spechit: Vec<i64>,
}

fn wrap32(v: i64) -> i64 {
    i64::from(v as i32)
}

impl Level {
    fn finesine(&self, fine: i64) -> i64 {
        self.finesine[(fine & FINEMASK) as usize]
    }

    fn finecosine(&self, fine: i64) -> i64 {
        self.finesine[((fine + QUARTER_TURN) & FINEMASK) as usize]
    }

    /// `P_LineOpening`'s `(opentop, openbottom)`. A one-sided line has no
    /// opening and nothing reads what the engine leaves there.
    fn opening(&self, line: usize) -> (i64, i64) {
        if self.line_side1[line] == -1 {
            return (0, 0);
        }
        let front = self.line_front[line] as usize;
        let back = self.line_back[line] as usize;
        (
            self.ceilingheight[front].min(self.ceilingheight[back]),
            self.floorheight[front].max(self.floorheight[back]),
        )
    }

    /// Where a sector height a line's two sides carry sits, with 0 for the
    /// side the engine reads as a null sector.
    fn side_height(&self, heights: &[i64], side: i64) -> i64 {
        if side < 0 { 0 } else { heights[side as usize] }
    }

    /// `P_AimLineAttack`: the slope a shot has to leave at, and the
    /// one-based slot it would reach, 0 for none.
    pub fn aim(&self, ask: &Ask) -> (i64, i64) {
        let fine = i64::from(ask.angle >> ANGLETOFINESHIFT);
        let reach = ask.range >> FRACBITS;
        let x2 = wrap32(ask.x + wrap32(reach * self.finecosine(fine)));
        let y2 = wrap32(ask.y + wrap32(reach * self.finesine(fine)));
        let shootz = wrap32(ask.z + (ask.height >> 1) + 8 * FRACUNIT);

        let mut topslope = TOPSLOPE;
        let mut bottomslope = -TOPSLOPE;
        for (id, frac, is_line) in self.map.traverse(ask.x, ask.y, x2, y2) {
            let dist = fixed_mul(ask.range, frac);
            if is_line == 1 {
                let line = id as usize;
                if self.line_flags[line] & ML_TWOSIDED == 0 {
                    return (0, 0);
                }
                let (opentop, openbottom) = self.opening(line);
                if openbottom >= opentop {
                    return (0, 0);
                }
                let backless = self.line_back[line] == -1;
                let front = self.line_front[line];
                let back = self.line_back[line];
                if backless
                    || self.side_height(&self.floorheight, front)
                        != self.side_height(&self.floorheight, back)
                {
                    bottomslope = bottomslope.max(fixed_div(openbottom - shootz, dist));
                }
                if backless
                    || self.side_height(&self.ceilingheight, front)
                        != self.side_height(&self.ceilingheight, back)
                {
                    topslope = topslope.min(fixed_div(opentop - shootz, dist));
                }
                if topslope <= bottomslope {
                    return (0, 0);
                }
                continue;
            }
            let slot = id as usize - 1;
            if i64::from(id) == ask.shooter || self.m_flags[slot] & MF_SHOOTABLE == 0 {
                continue;
            }
            let thingtop = fixed_div(wrap32(self.m_z[slot] + self.m_height[slot] - shootz), dist);
            if thingtop < bottomslope {
                continue;
            }
            let thingbottom = fixed_div(wrap32(self.m_z[slot] - shootz), dist);
            if thingbottom > topslope {
                continue;
            }
            let top = thingtop.min(topslope);
            let bottom = thingbottom.max(bottomslope);
            return ((top + bottom) / 2, i64::from(id));
        }
        (0, 0)
    }

    /// `P_LineAttack`: what the shot ends on, and where the puff or the
    /// blood goes.
    pub fn shoot(&self, ask: &Ask) -> Shot {
        let fine = i64::from(ask.angle >> ANGLETOFINESHIFT);
        let reach = ask.range >> FRACBITS;
        let x2 = wrap32(ask.x + wrap32(reach * self.finecosine(fine)));
        let y2 = wrap32(ask.y + wrap32(reach * self.finesine(fine)));
        let shootz = wrap32(ask.z + (ask.height >> 1) + 8 * FRACUNIT);
        let (tx, ty) = (nudge(ask.x, self.map.orgx), nudge(ask.y, self.map.orgy));
        let (tdx, tdy) = (wrap32(x2 - tx), wrap32(y2 - ty));
        let spot = |frac: i64| {
            (
                wrap32(tx + fixed_mul(tdx, frac)),
                wrap32(ty + fixed_mul(tdy, frac)),
                wrap32(shootz + fixed_mul(ask.slope, fixed_mul(frac, ask.range))),
            )
        };
        let mut spechit = Vec::new();
        for (id, frac, is_line) in self.map.traverse(ask.x, ask.y, x2, y2) {
            let dist = fixed_mul(ask.range, frac);
            if is_line == 1 {
                let line = id as usize;
                if self.line_special[line] != 0 {
                    spechit.push(i64::from(id));
                }
                if !self.line_stops(line, shootz, dist, ask.slope) {
                    continue;
                }
                let (x, y, z) = spot(wrap32(frac - fixed_div(4 * FRACUNIT, ask.range)));
                let front = self.line_front[line] as usize;
                let back = self.line_back[line];
                if self.ceilingpic[front] == self.skyflatnum
                    && (z > self.ceilingheight[front]
                        || (back != -1 && self.ceilingpic[back as usize] == self.skyflatnum))
                {
                    return Shot {
                        kind: 0,
                        id: 0,
                        x: 0,
                        y: 0,
                        z: 0,
                        spechit,
                    };
                }
                return Shot {
                    kind: 1,
                    id: i64::from(id),
                    x,
                    y,
                    z,
                    spechit,
                };
            }
            let slot = id as usize - 1;
            if i64::from(id) == ask.shooter || self.m_flags[slot] & MF_SHOOTABLE == 0 {
                continue;
            }
            let top = fixed_div(wrap32(self.m_z[slot] + self.m_height[slot] - shootz), dist);
            if top < ask.slope {
                continue;
            }
            let bottom = fixed_div(wrap32(self.m_z[slot] - shootz), dist);
            if bottom > ask.slope {
                continue;
            }
            let (x, y, z) = spot(wrap32(frac - fixed_div(10 * FRACUNIT, ask.range)));
            return Shot {
                kind: 2,
                id: i64::from(id),
                x,
                y,
                z,
                spechit,
            };
        }
        Shot {
            kind: 0,
            id: 0,
            x: 0,
            y: 0,
            z: 0,
            spechit,
        }
    }

    /// Whether a line the shot crosses stops it.
    fn line_stops(&self, line: usize, shootz: i64, dist: i64, slope: i64) -> bool {
        if self.line_flags[line] & ML_TWOSIDED == 0 {
            return true;
        }
        let (opentop, openbottom) = self.opening(line);
        let backless = self.line_back[line] == -1;
        let (front, back) = (self.line_front[line], self.line_back[line]);
        let steps =
            |of: &[i64]| backless || self.side_height(of, front) != self.side_height(of, back);
        if steps(&self.floorheight) && fixed_div(openbottom - shootz, dist) > slope {
            return true;
        }
        steps(&self.ceilingheight) && fixed_div(opentop - shootz, dist) < slope
    }

    /// `P_BulletSlope`: straight ahead, then a swing each way.
    pub fn bullet_slope(&self, ask: &Ask) -> (i64, i64) {
        let turned = |by: u32| Ask {
            angle: ask.angle.wrapping_add(by),
            range: AIMRANGE,
            ..*ask
        };
        let ahead = self.aim(&turned(0));
        if ahead.1 != 0 {
            return ahead;
        }
        let left = self.aim(&turned(AIMSWING));
        if left.1 != 0 {
            return left;
        }
        self.aim(&turned(AIMSWING.wrapping_neg()))
    }
}
#[cfg(feature = "clickhouse-tests")]
#[derive(Row, Deserialize)]
struct Header {
    origin_x: i32,
    origin_y: i32,
    columns: u32,
    rows: u32,
}

#[cfg(feature = "clickhouse-tests")]
#[derive(Row, Deserialize)]
struct Block {
    lines: Vec<u16>,
}

#[cfg(feature = "clickhouse-tests")]
#[derive(Row, Deserialize)]
struct Line {
    v1x: i32,
    v1y: i32,
    v2x: i32,
    v2y: i32,
    dx: i32,
    dy: i32,
    flags: i16,
    side1: i32,
    sector0: i32,
    sector1: i32,
}

#[cfg(feature = "clickhouse-tests")]
#[derive(Row, Deserialize)]
struct Sector {
    floorheight: i32,
    ceilingheight: i32,
    ceilingpic: i16,
}

#[cfg(feature = "clickhouse-tests")]
#[derive(Row, Deserialize)]
struct Mobjs {
    m_x: Vec<i32>,
    m_y: Vec<i32>,
    m_z: Vec<i32>,
    m_radius: Vec<i32>,
    m_height: Vec<i32>,
    m_flags: Vec<i32>,
    m_linkseq: Vec<u32>,
    line_special: Vec<i16>,
}

#[cfg(feature = "clickhouse-tests")]
#[derive(Row, Deserialize)]
struct Wave {
    value: i32,
}

#[cfg(feature = "clickhouse-tests")]
#[derive(Row, Deserialize)]
struct Sky {
    flatnum: i32,
}

/// The level and the mobjs the first state row leaves on it.
#[cfg(feature = "clickhouse-tests")]
pub async fn read_level(fixture: &Fixture) -> Level {
    let db = &fixture.database;
    let header: Header = fixture
        .scalar(&format!(
            "SELECT origin_x, origin_y, columns, rows FROM {db}.lv_blockmap_header LIMIT 1"
        ))
        .await;
    let blocks: Vec<Block> = fixture
        .rows(&format!("SELECT lines FROM {db}.lv_blockmap ORDER BY cell"))
        .await;
    let lines: Vec<Line> = fixture
        .rows(&format!(
            "SELECT a.x AS v1x, a.y AS v1y, b.x AS v2x, b.y AS v2y, l.dx AS dx, l.dy AS dy, \
             l.flags AS flags, l.side1 AS side1, l.sector0 AS sector0, l.sector1 AS sector1 \
             FROM {db}.lv_lines AS l \
             INNER JOIN {db}.lv_vertexes AS a ON a.id = l.v1 \
             INNER JOIN {db}.lv_vertexes AS b ON b.id = l.v2 \
             ORDER BY l.id"
        ))
        .await;
    let sectors: Vec<Sector> = fixture
        .rows(&format!(
            "SELECT floorheight, ceilingheight, ceilingpic \
             FROM {db}.lv_sectors_static ORDER BY id"
        ))
        .await;
    let mobjs: Mobjs = fixture
        .scalar(&format!(
            "SELECT m_x, m_y, m_z, m_radius, m_height, m_flags, m_linkseq, line_special \
             FROM {db}.native_state WHERE tic = 0"
        ))
        .await;
    let sky: Sky = fixture
        .scalar(&format!(
            "SELECT toInt32(id) AS flatnum FROM {db}.flats WHERE upper(name) = 'F_SKY1'"
        ))
        .await;
    let waves: Vec<Wave> = fixture
        .rows(&format!(
            "SELECT value FROM {db}.finesine WHERE id < 8192 ORDER BY id"
        ))
        .await;

    let map = Map {
        things: (0..mobjs.m_x.len())
            .map(|slot| Thing {
                x: i64::from(mobjs.m_x[slot]),
                y: i64::from(mobjs.m_y[slot]),
                radius: i64::from(mobjs.m_radius[slot]),
                linkseq: i64::from(mobjs.m_linkseq[slot]),
                alive: true,
            })
            .collect(),
        orgx: i64::from(header.origin_x),
        orgy: i64::from(header.origin_y),
        cols: i64::from(header.columns),
        rows: i64::from(header.rows),
        blocks: blocks
            .into_iter()
            .map(|block| block.lines.into_iter().map(i32::from).collect())
            .collect(),
        v1x: lines.iter().map(|l| i64::from(l.v1x)).collect(),
        v1y: lines.iter().map(|l| i64::from(l.v1y)).collect(),
        v2x: lines.iter().map(|l| i64::from(l.v2x)).collect(),
        v2y: lines.iter().map(|l| i64::from(l.v2y)).collect(),
        dx: lines.iter().map(|l| i64::from(l.dx)).collect(),
        dy: lines.iter().map(|l| i64::from(l.dy)).collect(),
    };
    Level {
        map,
        finesine: waves.into_iter().map(|w| i64::from(w.value)).collect(),
        line_flags: lines.iter().map(|l| i64::from(l.flags)).collect(),
        line_front: lines.iter().map(|l| i64::from(l.sector0)).collect(),
        line_back: lines.iter().map(|l| i64::from(l.sector1)).collect(),
        line_side1: lines.iter().map(|l| i64::from(l.side1)).collect(),
        line_special: mobjs.line_special.iter().map(|v| i64::from(*v)).collect(),
        ceilingpic: sectors.iter().map(|s| i64::from(s.ceilingpic)).collect(),
        skyflatnum: i64::from(sky.flatnum),
        floorheight: sectors.iter().map(|s| i64::from(s.floorheight)).collect(),
        ceilingheight: sectors.iter().map(|s| i64::from(s.ceilingheight)).collect(),
        m_z: mobjs.m_z.iter().map(|v| i64::from(*v)).collect(),
        m_height: mobjs.m_height.iter().map(|v| i64::from(*v)).collect(),
        m_flags: mobjs.m_flags.iter().map(|v| i64::from(*v)).collect(),
    }
}

/// The level's arrays as SQL literals, which is how a test hands them to
/// the generator.
pub struct Arrays {
    pub m_x: String,
    pub m_y: String,
    pub m_z: String,
    pub m_radius: String,
    pub m_height: String,
    pub m_flags: String,
    pub m_linkseq: String,
    pub alive: String,
    pub floorheight: String,
    pub ceilingheight: String,
    pub line_flags: String,
    pub line_special: String,
    pub ceilingpic: String,
}

fn literal(of: &[i64]) -> String {
    format!(
        "[{}]",
        of.iter().map(i64::to_string).collect::<Vec<_>>().join(", ")
    )
}

impl Level {
    pub fn arrays(&self) -> Arrays {
        let mobj = |of: &dyn Fn(&Thing) -> i64| {
            literal(&self.map.things.iter().map(of).collect::<Vec<_>>())
        };
        Arrays {
            m_x: mobj(&|thing| thing.x),
            m_y: mobj(&|thing| thing.y),
            m_z: literal(&self.m_z),
            m_radius: mobj(&|thing| thing.radius),
            m_height: literal(&self.m_height),
            m_flags: literal(&self.m_flags),
            m_linkseq: mobj(&|thing| thing.linkseq),
            alive: mobj(&|thing| i64::from(thing.alive)),
            floorheight: literal(&self.floorheight),
            ceilingheight: literal(&self.ceilingheight),
            line_flags: literal(&self.line_flags),
            line_special: literal(&self.line_special),
            ceilingpic: literal(&self.ceilingpic),
        }
    }
}
