//! `P_CheckSight` in Rust, from `p_sight.c`, for the live test to compare
//! against.
//!
//! This is the engine's own shape: the `REJECT` test, then
//! `P_CrossBSPNode` descending from the root and `P_CrossSubsector`
//! walking each leaf's segs with `validcount` and the early returns. The
//! SQL says the same thing without a walk, so the two agreeing is what
//! makes that reformulation worth trusting.

const FRACBITS: u32 = 16;
const ML_TWOSIDED: i64 = 4;
const NF_SUBSECTOR: u32 = 0x8000;

/// A thing a sight check is about.
#[derive(Clone, Copy, Debug)]
pub struct Thing {
    pub x: i64,
    pub y: i64,
    pub z: i64,
    pub height: i64,
}

/// The level, as the arrays `p_sight.c` reads.
pub struct Map {
    pub node_x: Vec<i64>,
    pub node_y: Vec<i64>,
    pub node_dx: Vec<i64>,
    pub node_dy: Vec<i64>,
    pub node_child: Vec<[u32; 2]>,
    /// `firstline` and `numlines` per subsector.
    pub subsector: Vec<(usize, usize)>,
    pub ssec_sector: Vec<usize>,
    pub seg_line: Vec<usize>,
    pub seg_front: Vec<i64>,
    pub seg_back: Vec<i64>,
    pub line_v1x: Vec<i64>,
    pub line_v1y: Vec<i64>,
    pub line_v2x: Vec<i64>,
    pub line_v2y: Vec<i64>,
    pub line_dx: Vec<i64>,
    pub line_dy: Vec<i64>,
    pub line_flags: Vec<i64>,
    pub floorheight: Vec<i64>,
    pub ceilingheight: Vec<i64>,
    pub reject: Vec<u8>,
    pub numsectors: usize,
}

/// The statics `P_CheckSight` leaves for the two walkers.
struct Trace {
    x: i64,
    y: i64,
    dx: i64,
    dy: i64,
    t2x: i64,
    t2y: i64,
    zstart: i64,
    topslope: i64,
    bottomslope: i64,
    validcount: Vec<bool>,
}

impl Map {
    /// `R_PointInSubsector`.
    pub fn point_in_subsector(&self, x: i64, y: i64) -> usize {
        if self.node_x.is_empty() {
            return 0;
        }
        let mut at = (self.node_x.len() - 1) as u32;
        while at & NF_SUBSECTOR == 0 {
            let n = at as usize;
            let side = point_on_side(
                x,
                y,
                self.node_x[n],
                self.node_y[n],
                self.node_dx[n],
                self.node_dy[n],
            );
            at = self.node_child[n][side];
        }
        (at & (NF_SUBSECTOR - 1)) as usize
    }

    /// `P_CheckSight`.
    pub fn check_sight(&self, t1: Thing, t2: Thing) -> bool {
        let s1 = self.ssec_sector[self.point_in_subsector(t1.x, t1.y)];
        let s2 = self.ssec_sector[self.point_in_subsector(t2.x, t2.y)];
        let pnum = s1 * self.numsectors + s2;
        if self.reject[pnum >> 3] & (1 << (pnum & 7)) != 0 {
            return false;
        }
        let zstart = wrap(t1.z + t1.height - (t1.height >> 2));
        let mut trace = Trace {
            x: t1.x,
            y: t1.y,
            dx: wrap(t2.x - t1.x),
            dy: wrap(t2.y - t1.y),
            t2x: t2.x,
            t2y: t2.y,
            zstart,
            topslope: wrap(t2.z + t2.height - zstart),
            bottomslope: wrap(t2.z - zstart),
            validcount: vec![false; self.line_v1x.len()],
        };
        self.cross_node((self.node_x.len() - 1) as u32, &mut trace)
    }

    /// `P_CrossBSPNode`.
    fn cross_node(&self, at: u32, trace: &mut Trace) -> bool {
        if at & NF_SUBSECTOR != 0 {
            return self.cross_subsector((at & (NF_SUBSECTOR - 1)) as usize, trace);
        }
        let n = at as usize;
        let (nx, ny, ndx, ndy) = (
            self.node_x[n],
            self.node_y[n],
            self.node_dx[n],
            self.node_dy[n],
        );
        let mut side = divline_side(trace.x, trace.y, nx, ny, ndx, ndy);
        if side == 2 {
            side = 0;
        }
        if !self.cross_node(self.node_child[n][side], trace) {
            return false;
        }
        if side == divline_side(trace.t2x, trace.t2y, nx, ny, ndx, ndy) {
            return true;
        }
        self.cross_node(self.node_child[n][side ^ 1], trace)
    }

    /// `P_CrossSubsector`.
    fn cross_subsector(&self, num: usize, trace: &mut Trace) -> bool {
        let (first, count) = self.subsector[num];
        for seg in first..first + count {
            let line = self.seg_line[seg];
            if trace.validcount[line] {
                continue;
            }
            trace.validcount[line] = true;
            let (v1x, v1y) = (self.line_v1x[line], self.line_v1y[line]);
            let (v2x, v2y) = (self.line_v2x[line], self.line_v2y[line]);
            let s1 = divline_side(v1x, v1y, trace.x, trace.y, trace.dx, trace.dy);
            let s2 = divline_side(v2x, v2y, trace.x, trace.y, trace.dx, trace.dy);
            if s1 == s2 {
                continue;
            }
            let (ldx, ldy) = (self.line_dx[line], self.line_dy[line]);
            let s1 = divline_side(trace.x, trace.y, v1x, v1y, ldx, ldy);
            let s2 = divline_side(trace.t2x, trace.t2y, v1x, v1y, ldx, ldy);
            if s1 == s2 {
                continue;
            }
            if self.line_flags[line] & ML_TWOSIDED == 0 {
                return false;
            }
            let front = self.seg_front[seg] as usize;
            let back = self.seg_back[seg] as usize;
            if self.floorheight[front] == self.floorheight[back]
                && self.ceilingheight[front] == self.ceilingheight[back]
            {
                continue;
            }
            let opentop = self.ceilingheight[front].min(self.ceilingheight[back]);
            let openbottom = self.floorheight[front].max(self.floorheight[back]);
            if openbottom >= opentop {
                return false;
            }
            let frac = intercept_vector(trace, v1x, v1y, ldx, ldy);
            if self.floorheight[front] != self.floorheight[back] {
                let slope = fixed_div(wrap(openbottom - trace.zstart), frac);
                if slope > trace.bottomslope {
                    trace.bottomslope = slope;
                }
            }
            if self.ceilingheight[front] != self.ceilingheight[back] {
                let slope = fixed_div(wrap(opentop - trace.zstart), frac);
                if slope < trace.topslope {
                    trace.topslope = slope;
                }
            }
            if trace.topslope <= trace.bottomslope {
                return false;
            }
        }
        true
    }
}

/// `P_InterceptVector2`, with the trace as `v2` and the line as `v1`.
fn intercept_vector(trace: &Trace, lx: i64, ly: i64, ldx: i64, ldy: i64) -> i64 {
    let den = wrap(fixed_mul(ldy >> 8, trace.dx) - fixed_mul(ldx >> 8, trace.dy));
    if den == 0 {
        return 0;
    }
    let num =
        wrap(fixed_mul(wrap(lx - trace.x) >> 8, ldy) + fixed_mul(wrap(trace.y - ly) >> 8, ldx));
    fixed_div(num, den)
}

/// `P_DivlineSide`, second case and all. The `x == node->y` is the
/// engine's.
fn divline_side(x: i64, y: i64, lx: i64, ly: i64, ldx: i64, ldy: i64) -> usize {
    if ldx == 0 {
        if x == lx {
            return 2;
        }
        if x <= lx {
            return usize::from(ldy > 0);
        }
        return usize::from(ldy < 0);
    }
    if ldy == 0 {
        if x == ly {
            return 2;
        }
        if y <= ly {
            return usize::from(ldx < 0);
        }
        return usize::from(ldx > 0);
    }
    let left = wrap((ldy >> FRACBITS) * (wrap(x - lx) >> FRACBITS));
    let right = wrap((wrap(y - ly) >> FRACBITS) * (ldx >> FRACBITS));
    if right < left {
        return 0;
    }
    if left == right {
        return 2;
    }
    1
}

/// `R_PointOnSide`, which the subsector lookup needs.
fn point_on_side(x: i64, y: i64, lx: i64, ly: i64, ldx: i64, ldy: i64) -> usize {
    if ldx == 0 {
        return if x <= lx {
            usize::from(ldy > 0)
        } else {
            usize::from(ldy < 0)
        };
    }
    if ldy == 0 {
        return if y <= ly {
            usize::from(ldx < 0)
        } else {
            usize::from(ldx > 0)
        };
    }
    let dx = wrap(x - lx);
    let dy = wrap(y - ly);
    // The engine decides by the sign bits where it can.
    if (ldy ^ ldx ^ dx ^ dy) & 0x8000_0000 != 0 {
        return usize::from((ldy ^ dx) & 0x8000_0000 != 0);
    }
    let left = fixed_mul(ldy >> 16, dx);
    let right = fixed_mul(dy, ldx >> 16);
    usize::from(right >= left)
}

fn fixed_mul(a: i64, b: i64) -> i64 {
    wrap((a * b) >> 16)
}

fn fixed_div(a: i64, b: i64) -> i64 {
    if (a.abs() >> 14) >= b.abs() {
        return if (a ^ b) < 0 {
            i32::MIN.into()
        } else {
            i32::MAX.into()
        };
    }
    wrap((a << 16) / b)
}

/// What a `fixed_t` holds after the arithmetic above wraps.
fn wrap(v: i64) -> i64 {
    i64::from(v as i32)
}
