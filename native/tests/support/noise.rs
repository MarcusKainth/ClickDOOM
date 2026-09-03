//! `P_NoiseAlert` in Rust, from `p_enemy.c`.
//!
//! The SQL floods the sectors twice rather than recursing, so the walk it
//! does not do is written out here and the two are compared.

/// `doomdata.h`
const ML_TWOSIDED: i64 = 4;
const ML_SOUNDBLOCK: i64 = 64;

/// What the walk reads: the lines around each sector and the two sectors
/// each line joins.
pub struct Map {
    /// `sec->lines`, by sector.
    pub sector_lines: Vec<Vec<usize>>,
    pub line_flags: Vec<i64>,
    /// `sides[sidenum[0]].sector` and `sides[sidenum[1]].sector`. A line
    /// carrying `ML_TWOSIDED` has both.
    pub line_front: Vec<usize>,
    pub line_back: Vec<usize>,
}

impl Map {
    /// `P_NoiseAlert`: `soundtraversed` for every sector once the flood
    /// has run, and 0 for a sector it does not reach.
    pub fn noise_alert(&self, emitter: usize, floor: &[i32], ceiling: &[i32]) -> Vec<u8> {
        let mut walk = Walk {
            map: self,
            floor,
            ceiling,
            traversed: vec![0; self.sector_lines.len()],
            flooded: vec![false; self.sector_lines.len()],
        };
        walk.recursive_sound(emitter, 0);
        walk.traversed
    }
}

/// One flood. `flooded` is the engine's `sec->validcount == validcount`,
/// which is per alert.
struct Walk<'a> {
    map: &'a Map,
    floor: &'a [i32],
    ceiling: &'a [i32],
    traversed: Vec<u8>,
    flooded: Vec<bool>,
}

impl Walk<'_> {
    fn recursive_sound(&mut self, sector: usize, soundblocks: u8) {
        if self.flooded[sector] && self.traversed[sector] <= soundblocks + 1 {
            return;
        }
        self.flooded[sector] = true;
        self.traversed[sector] = soundblocks + 1;
        for at in 0..self.map.sector_lines[sector].len() {
            let line = self.map.sector_lines[sector][at];
            if self.map.line_flags[line] & ML_TWOSIDED == 0 {
                continue;
            }
            let (front, back) = (self.map.line_front[line], self.map.line_back[line]);
            let open = self.ceiling[front].min(self.ceiling[back])
                - self.floor[front].max(self.floor[back]);
            if open <= 0 {
                continue;
            }
            let other = if front == sector { back } else { front };
            if self.map.line_flags[line] & ML_SOUNDBLOCK == 0 {
                self.recursive_sound(other, soundblocks);
            } else if soundblocks == 0 {
                self.recursive_sound(other, 1);
            }
        }
    }
}
