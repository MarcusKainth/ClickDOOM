//! The light thinkers, read a second time.
//!
//! This is an oracle and nothing else. Native mode runs them in SQL; this
//! follows `p_lights.c` instead, so the two agreeing means something.

/// `p_spec.h`
const GLOWSPEED: i32 = 8;
/// `p_lights.c`
const FLICKER_COUNT: i32 = 4;

/// `clickdoom_spec::native_state::sector_thinker_kind`
const LIGHT_FLASH: u8 = 5;
const STROBE: u8 = 6;
const GLOW: u8 = 7;
const FIRE_FLICKER: u8 = 8;

/// One light thinker, as the state row holds it.
pub struct Thinker {
    pub sector: i32,
    pub kind: u8,
    pub count: i32,
    pub direction: i32,
    pub minlight: i32,
    pub maxlight: i32,
    pub mintime: i32,
    pub maxtime: i32,
}

/// Everything a tic of light thinkers reads and writes.
pub struct Lights {
    pub lightlevel: Vec<i16>,
    pub thinkers: Vec<Thinker>,
    pub prndindex: u32,
}

impl Lights {
    /// `P_RunThinkers` over the sector thinkers, in list order.
    pub fn tic(&mut self, rnd: &[i64]) {
        for thinker in &mut self.thinkers {
            let at = thinker.sector as usize;
            let level = i32::from(self.lightlevel[at]);
            let mut draw = || {
                self.prndindex = (self.prndindex + 1) & 0xff;
                rnd[self.prndindex as usize] as i32
            };
            match thinker.kind {
                GLOW => {
                    if thinker.direction == -1 {
                        if level - GLOWSPEED <= thinker.minlight {
                            thinker.direction = 1;
                        } else {
                            self.lightlevel[at] = (level - GLOWSPEED) as i16;
                        }
                    } else if thinker.direction == 1 {
                        if level + GLOWSPEED >= thinker.maxlight {
                            thinker.direction = -1;
                        } else {
                            self.lightlevel[at] = (level + GLOWSPEED) as i16;
                        }
                    }
                }
                kind => {
                    thinker.count -= 1;
                    if thinker.count != 0 {
                        continue;
                    }
                    match kind {
                        LIGHT_FLASH => {
                            if level == thinker.maxlight {
                                self.lightlevel[at] = thinker.minlight as i16;
                                thinker.count = (draw() & thinker.mintime) + 1;
                            } else {
                                self.lightlevel[at] = thinker.maxlight as i16;
                                thinker.count = (draw() & thinker.maxtime) + 1;
                            }
                        }
                        STROBE => {
                            if level == thinker.minlight {
                                self.lightlevel[at] = thinker.maxlight as i16;
                                thinker.count = thinker.maxtime;
                            } else {
                                self.lightlevel[at] = thinker.minlight as i16;
                                thinker.count = thinker.mintime;
                            }
                        }
                        FIRE_FLICKER => {
                            let amount = (draw() & 3) * 16;
                            self.lightlevel[at] = if level - amount < thinker.minlight {
                                thinker.minlight as i16
                            } else {
                                (thinker.maxlight - amount) as i16
                            };
                            thinker.count = FLICKER_COUNT;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
