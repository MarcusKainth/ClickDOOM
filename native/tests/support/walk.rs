//! The player's push and view height, read a second time.
//!
//! This is an oracle and nothing else. Native mode moves the player in
//! SQL; this follows `p_user.c` and `p_mobj.c` instead, for the part of
//! the move that does not ask the blockmap anything: the thrust, the
//! friction and the bob. Where the player walks in the first corridor of
//! `DEMO3` nothing blocks it, so `P_TryMove` always says yes.

use xxhash_rust::xxh64::xxh64;

/// `info.h`: the first of the player's four walking frames.
pub const S_PLAY_RUN1: i32 = 150;

/// `p_local.h`
const VIEWHEIGHT: i64 = 41 << 16;
const MAXBOB: i64 = 0x10_0000;
/// `p_mobj.c`
const STOPSPEED: i64 = 0x1000;
const FRICTION: i64 = 0xe800;
/// `tables.h`
const ANG90: u32 = 0x4000_0000;
const ANGLETOFINESHIFT: u32 = 19;
const FINEMASK: usize = 8191;

fn fixed_mul(a: i64, b: i64) -> i64 {
    (a * b) >> 16
}

fn finecosine(finesine: &[i64], angle: usize) -> i64 {
    finesine[(angle + 2048) & FINEMASK]
}

/// The player between tics.
pub struct Player {
    pub x: i32,
    pub y: i32,
    pub angle: u32,
    pub momx: i32,
    pub momy: i32,
    pub viewheight: i32,
    pub deltaviewheight: i32,
}

/// What one tic leaves for the frame to draw.
pub struct Step {
    pub bob: i32,
    pub viewz: i32,
}

impl Player {
    /// `P_MovePlayer`, `P_CalcHeight` and `P_XYMovement`, in the order
    /// `P_Ticker` runs them.
    pub fn tic(
        &mut self,
        finesine: &[i64],
        forwardmove: i8,
        sidemove: i8,
        angleturn: i16,
        leveltime: i32,
    ) -> Step {
        self.angle = self.angle.wrapping_add((i64::from(angleturn) << 16) as u32);
        let mut thrust = |angle: u32, move_: i64| {
            let fine = (angle >> ANGLETOFINESHIFT) as usize;
            self.momx = self
                .momx
                .wrapping_add(fixed_mul(move_, finecosine(finesine, fine)) as i32);
            self.momy = self
                .momy
                .wrapping_add(fixed_mul(move_, finesine[fine & FINEMASK]) as i32);
        };
        if forwardmove != 0 {
            thrust(self.angle, i64::from(forwardmove) * 2048);
        }
        if sidemove != 0 {
            thrust(self.angle.wrapping_sub(ANG90), i64::from(sidemove) * 2048);
        }

        // P_CalcHeight, from the momentum the push left.
        let square = fixed_mul(i64::from(self.momx), i64::from(self.momx))
            + fixed_mul(i64::from(self.momy), i64::from(self.momy));
        let bob = (square >> 2).min(MAXBOB);
        let angle = ((8192 / 20) * i64::from(leveltime)) as usize & FINEMASK;
        let bobamt = fixed_mul(bob / 2, finesine[angle]);
        let raised = i64::from(self.viewheight) + i64::from(self.deltaviewheight);
        if raised > VIEWHEIGHT {
            self.viewheight = VIEWHEIGHT as i32;
            self.deltaviewheight = 0;
        } else if raised < VIEWHEIGHT / 2 {
            self.viewheight = (VIEWHEIGHT / 2) as i32;
            if self.deltaviewheight <= 0 {
                self.deltaviewheight = 1;
            }
        } else {
            self.viewheight = raised as i32;
        }
        if self.deltaviewheight != 0 {
            self.deltaviewheight += 1 << 14;
            if self.deltaviewheight == 0 {
                self.deltaviewheight = 1;
            }
        }
        let viewz = (i64::from(self.viewheight) + bobamt) as i32;

        // P_XYMovement, for a move nothing blocks.
        self.x = self.x.wrapping_add(self.momx);
        self.y = self.y.wrapping_add(self.momy);
        let stopped = i64::from(self.momx) > -STOPSPEED
            && i64::from(self.momx) < STOPSPEED
            && i64::from(self.momy) > -STOPSPEED
            && i64::from(self.momy) < STOPSPEED
            && forwardmove == 0
            && sidemove == 0;
        if stopped {
            self.momx = 0;
            self.momy = 0;
        } else {
            self.momx = fixed_mul(i64::from(self.momx), FRICTION) as i32;
            self.momy = fixed_mul(i64::from(self.momy), FRICTION) as i32;
        }
        Step {
            bob: bob as i32,
            viewz,
        }
    }
}

/// A message as both writers hash one.
pub fn message(text: &str) -> u64 {
    xxh64(text.as_bytes(), clickdoom_spec::XXH64_SEED)
}
