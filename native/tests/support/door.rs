//! `T_VerticalDoor`, read from `p_doors.c`, for the two kinds a walk-over
//! crossing spawns: `normal` (open, wait, close) and `open` (open and stay).

/// `p_doors.c`
const VDOORSPEED: i32 = 2 << 16;
const VDOORWAIT: i32 = 150;

/// `p_doors.h`: `vldoor_e`, the two kinds a crossing spawns.
const NORMAL: i32 = 0;
const OPEN: i32 = 3;

/// What one tic of the door leaves behind.
pub struct Step {
    pub ceilingheight: i32,
    pub direction: i32,
    pub count: i32,
}

/// A door part way through its run.
pub struct Door {
    kind: i32,
    ceilingheight: i32,
    topheight: i32,
    floorheight: i32,
    speed: i32,
    wait: i32,
    count: i32,
    direction: i32,
    /// Whether the thinker has come off the list.
    pub done: bool,
    pub opened: bool,
    pub waited: bool,
    pub closed: bool,
}

impl Door {
    fn new(kind: i32, ceilingheight: i32, topheight: i32, floorheight: i32) -> Door {
        Door {
            kind,
            ceilingheight,
            topheight,
            floorheight,
            speed: VDOORSPEED,
            wait: VDOORWAIT,
            count: 0,
            direction: 1,
            done: false,
            opened: false,
            waited: false,
            closed: false,
        }
    }

    /// `EV_DoDoor(line, vld_normal)`: opens, waits at the top, then closes.
    pub fn normal(ceilingheight: i32, topheight: i32, floorheight: i32) -> Door {
        Door::new(NORMAL, ceilingheight, topheight, floorheight)
    }

    /// `EV_DoDoor(line, vld_open)`: opens and stays open.
    pub fn open(ceilingheight: i32, topheight: i32, floorheight: i32) -> Door {
        Door::new(OPEN, ceilingheight, topheight, floorheight)
    }

    /// One `T_VerticalDoor`. The thinker is gone once `done` is set, the
    /// same tic this last runs.
    ///
    /// `T_MovePlane`'s own "one more step would pass the destination" test
    /// is strict (`p_floor.c:162`, `sector->ceilingheight + speed > dest`,
    /// not `>=`; `p_floor.c:128` reads the same way for a plane going
    /// down), so a distance the speed divides evenly reaches the
    /// destination one tic before `PASTDEST` registers: the tic that lands
    /// exactly on it still reports `OK`.
    pub fn tic(&mut self) -> Step {
        match self.direction {
            1 => {
                if self.ceilingheight + self.speed > self.topheight {
                    self.ceilingheight = self.topheight;
                    self.opened = true;
                    if self.kind == NORMAL {
                        self.direction = 0;
                        self.count = self.wait;
                    } else {
                        self.done = true;
                    }
                } else {
                    self.ceilingheight += self.speed;
                }
            }
            0 => {
                self.count -= 1;
                if self.count == 0 {
                    self.direction = -1;
                    self.waited = true;
                }
            }
            -1 => {
                if self.ceilingheight - self.speed < self.floorheight {
                    self.ceilingheight = self.floorheight;
                    self.closed = true;
                    self.done = true;
                } else {
                    self.ceilingheight -= self.speed;
                }
            }
            _ => {}
        }
        Step {
            ceilingheight: self.ceilingheight,
            direction: self.direction,
            count: self.count,
        }
    }
}
