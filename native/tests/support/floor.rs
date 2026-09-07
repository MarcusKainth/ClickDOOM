//! `T_MoveFloor`, read from `p_floor.c`, for the two crossing-triggered
//! kinds: `raiseFloor` (up, to the lowest ceiling surrounding, clamped to
//! the sector's own) and `turboLower` (down, to the highest floor
//! surrounding, 8 map units past it when that differs from the current
//! floor).

/// `p_spec.h`
const FLOORSPEED: i32 = 1 << 16;

/// What one tic of the floor leaves behind.
pub struct Step {
    pub floorheight: i32,
}

/// A floor part way through its run.
pub struct Floor {
    floorheight: i32,
    dest: i32,
    speed: i32,
    direction: i32,
    /// Whether the thinker has come off the list.
    pub done: bool,
}

impl Floor {
    fn new(floorheight: i32, dest: i32, speed: i32, direction: i32) -> Floor {
        Floor {
            floorheight,
            dest,
            speed,
            direction,
            done: false,
        }
    }

    /// `EV_DoFloor(line, raiseFloor)`: up, to `P_FindLowestCeilingSurrounding`,
    /// clamped by the caller to the sector's own ceiling.
    pub fn raise(floorheight: i32, dest: i32) -> Floor {
        Floor::new(floorheight, dest, FLOORSPEED, 1)
    }

    /// `EV_DoFloor(line, turboLower)`: down, to `P_FindHighestFloorSurrounding`,
    /// 8 map units past it when the caller finds that differs from the
    /// sector's own floor.
    pub fn turbo_lower(floorheight: i32, dest: i32) -> Floor {
        Floor::new(floorheight, dest, FLOORSPEED * 4, -1)
    }

    /// One `T_MoveFloor`. The thinker is gone once `done` is set, the same
    /// tic this last runs.
    ///
    /// `T_MovePlane`'s own "one more step would pass the destination" test
    /// is strict (`p_floor.c:90`, `sector->floorheight + speed > dest`, not
    /// `>=`; `p_floor.c:61` reads the same way for a floor going down), so
    /// a distance the speed divides evenly reaches the destination one tic
    /// before `PASTDEST` registers: the tic that lands exactly on it still
    /// reports `OK`.
    pub fn tic(&mut self) -> Step {
        if self.direction == 1 {
            if self.floorheight + self.speed > self.dest {
                self.floorheight = self.dest;
                self.done = true;
            } else {
                self.floorheight += self.speed;
            }
        } else if self.floorheight - self.speed < self.dest {
            self.floorheight = self.dest;
            self.done = true;
        } else {
            self.floorheight -= self.speed;
        }
        Step {
            floorheight: self.floorheight,
        }
    }
}
