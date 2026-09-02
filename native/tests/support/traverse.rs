//! `P_PathTraverse` with `PT_ADDLINES`, read a second time.
//!
//! This is an oracle and nothing else. Native mode walks the blockmap in
//! SQL; this follows `p_maputl.c` instead, so the two agreeing means
//! something.

const FRACBITS: u32 = 16;
const FRACUNIT: i64 = 1 << FRACBITS;
const MAPBLOCKSHIFT: u32 = FRACBITS + 7;
const MAPBTOFRAC: u32 = MAPBLOCKSHIFT - FRACBITS;
const MAPBLOCKSIZE: i64 = 1 << MAPBLOCKSHIFT;
/// `p_maputl.c`: how many blocks a trace walks before it gives up.
const BLOCKS: usize = 64;

fn fixed_mul(a: i64, b: i64) -> i64 {
    ((a as i32 as i64 * b as i32 as i64) >> FRACBITS) as i32 as i64
}

/// `FixedDiv`, with the saturation the engine's own has.
fn fixed_div(a: i64, b: i64) -> i64 {
    let (a, b) = (a as i32, b as i32);
    if (a.unsigned_abs() >> 14) >= b.unsigned_abs() {
        return if (a ^ b) < 0 { i32::MIN } else { i32::MAX } as i64;
    }
    ((i64::from(a) << FRACBITS) / i64::from(b)) as i32 as i64
}

/// `P_PointOnDivlineSide` and `P_PointOnLineSide` share this shape.
fn side(x: i64, y: i64, lx: i64, ly: i64, ldx: i64, ldy: i64, shift: u32) -> u8 {
    if ldx == 0 {
        return u8::from(if x <= lx { ldy > 0 } else { ldy < 0 });
    }
    if ldy == 0 {
        return u8::from(if y <= ly { ldx < 0 } else { ldx > 0 });
    }
    let dx = (x as i32).wrapping_sub(lx as i32) as i64;
    let dy = (y as i32).wrapping_sub(ly as i32) as i64;
    if shift == 8 {
        // The divline test looks at the sign bits first.
        if (ldy ^ ldx ^ dx ^ dy) as i32 & i32::MIN != 0 {
            return u8::from((ldy ^ dx) as i32 & i32::MIN != 0);
        }
    }
    let left = fixed_mul(ldy >> shift, dx >> if shift == 8 { 8 } else { 0 });
    let right = fixed_mul(dy >> if shift == 8 { 8 } else { 0 }, ldx >> shift);
    u8::from(right >= left)
}

/// `P_InterceptVector(v2, v1)`.
#[allow(clippy::too_many_arguments)]
fn intercept(
    v2x: i64,
    v2y: i64,
    v2dx: i64,
    v2dy: i64,
    v1x: i64,
    v1y: i64,
    v1dx: i64,
    v1dy: i64,
) -> i64 {
    let den = fixed_mul(v1dy >> 8, v2dx) - fixed_mul(v1dx >> 8, v2dy);
    if den == 0 {
        return 0;
    }
    let num = fixed_mul((v1x - v2x) >> 8, v1dy) + fixed_mul((v2y - v1y) >> 8, v1dx);
    fixed_div(num, den)
}

/// The level a trace walks over.
pub struct Map {
    pub orgx: i64,
    pub orgy: i64,
    pub cols: i64,
    pub rows: i64,
    /// One line list per block, `by * cols + bx`.
    pub blocks: Vec<Vec<i32>>,
    pub v1x: Vec<i64>,
    pub v1y: Vec<i64>,
    pub v2x: Vec<i64>,
    pub v2y: Vec<i64>,
    pub dx: Vec<i64>,
    pub dy: Vec<i64>,
}

impl Map {
    /// The lines the trace crosses, nearest first, with their fractions.
    pub fn traverse(&self, x1: i64, y1: i64, x2: i64, y2: i64) -> Vec<(i32, i64)> {
        let x1 = x1 + i64::from((x1 - self.orgx) & (MAPBLOCKSIZE - 1) == 0) * FRACUNIT;
        let y1 = y1 + i64::from((y1 - self.orgy) & (MAPBLOCKSIZE - 1) == 0) * FRACUNIT;
        let (tdx, tdy) = (x2 - x1, y2 - y1);
        let (rx1, ry1) = (x1 - self.orgx, y1 - self.orgy);
        let (rx2, ry2) = (x2 - self.orgx, y2 - self.orgy);
        let (xt1, yt1) = (rx1 >> MAPBLOCKSHIFT, ry1 >> MAPBLOCKSHIFT);
        let (xt2, yt2) = (rx2 >> MAPBLOCKSHIFT, ry2 >> MAPBLOCKSHIFT);

        let mapxstep = (xt2 - xt1).signum();
        let mapystep = (yt2 - yt1).signum();
        let ystep = if mapxstep == 0 {
            256 * FRACUNIT
        } else {
            fixed_div(ry2 - ry1, (rx2 - rx1).abs())
        };
        let xstep = if mapystep == 0 {
            256 * FRACUNIT
        } else {
            fixed_div(rx2 - rx1, (ry2 - ry1).abs())
        };
        let partial = |step: i64, rel: i64| match step {
            s if s > 0 => FRACUNIT - ((rel >> MAPBTOFRAC) & (FRACUNIT - 1)),
            s if s < 0 => (rel >> MAPBTOFRAC) & (FRACUNIT - 1),
            _ => FRACUNIT,
        };
        let mut yintercept = (ry1 >> MAPBTOFRAC) + fixed_mul(partial(mapxstep, rx1), ystep);
        let mut xintercept = (rx1 >> MAPBTOFRAC) + fixed_mul(partial(mapystep, ry1), xstep);

        let (mut mapx, mut mapy) = (xt1, yt1);
        let mut walked: Vec<i32> = Vec::new();
        for _ in 0..BLOCKS {
            if mapx >= 0 && mapx < self.cols && mapy >= 0 && mapy < self.rows {
                for line in &self.blocks[(mapy * self.cols + mapx) as usize] {
                    if !walked.contains(line) {
                        walked.push(*line);
                    }
                }
            }
            if mapx == xt2 && mapy == yt2 {
                break;
            }
            if (yintercept >> FRACBITS) == mapy {
                yintercept += ystep;
                mapx += mapxstep;
            } else if (xintercept >> FRACBITS) == mapx {
                xintercept += xstep;
                mapy += mapystep;
            }
        }

        let far = 16 * FRACUNIT;
        let long = tdx > far || tdy > far || tdx < -far || tdy < -far;
        let mut hits: Vec<(i32, i64)> = Vec::new();
        for line in walked {
            let at = line as usize;
            let (s1, s2) = if long {
                (
                    side(self.v1x[at], self.v1y[at], x1, y1, tdx, tdy, 8),
                    side(self.v2x[at], self.v2y[at], x1, y1, tdx, tdy, 8),
                )
            } else {
                (
                    side(
                        x1,
                        y1,
                        self.v1x[at],
                        self.v1y[at],
                        self.dx[at],
                        self.dy[at],
                        16,
                    ),
                    side(
                        x1 + tdx,
                        y1 + tdy,
                        self.v1x[at],
                        self.v1y[at],
                        self.dx[at],
                        self.dy[at],
                        16,
                    ),
                )
            };
            if s1 == s2 {
                continue;
            }
            let frac = intercept(
                x1,
                y1,
                tdx,
                tdy,
                self.v1x[at],
                self.v1y[at],
                self.dx[at],
                self.dy[at],
            );
            if !(0..=FRACUNIT).contains(&frac) {
                continue;
            }
            hits.push((line, frac));
        }
        // `P_TraverseIntercepts` takes the nearest each time, and a strict
        // comparison leaves an equal fraction where the walk put it.
        hits.sort_by_key(|(_, frac)| *frac);
        hits
    }
}
