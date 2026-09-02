//! `T_PlatRaise` and the part of `T_MovePlane` it drives, read from
//! `p_plats.c` and `p_floor.c`, and the row that puts one on the list.

use clickdoom_native::sql::sim;
use clickdoom_spec::native_state::sector_thinker_kind;

/// `p_spec.h`
const PLATSPEED: i32 = 1 << 16;
const PLATWAIT: i32 = 3;
const TICRATE: i32 = 35;

/// `p_spec.h`: the `plat_e` values.
const UP: i32 = 0;
const DOWN: i32 = 1;
const WAITING: i32 = 2;

/// `p_spec.h`: `downWaitUpStay` in `plattype_e`.
const DOWN_WAIT_UP_STAY: i32 = 1;

/// What one tic of the plat leaves behind.
pub struct Step {
    pub floorheight: i32,
    pub status: i32,
    pub count: i32,
}

/// A plat part way through its run.
pub struct Plat {
    floorheight: i32,
    low: i32,
    high: i32,
    speed: i32,
    wait: i32,
    count: i32,
    status: i32,
    /// Whether the run has covered each part, so a test can say it did.
    pub reached_bottom: bool,
    pub waited: bool,
}

impl Plat {
    /// `EV_DoPlat` for `downWaitUpStay`: it starts at the top going down.
    pub fn down_wait_up_stay(high: i32, low: i32) -> Plat {
        Plat {
            floorheight: high,
            low,
            high,
            speed: PLATSPEED * 4,
            wait: TICRATE * PLATWAIT,
            count: 0,
            status: DOWN,
            reached_bottom: false,
            waited: false,
        }
    }

    /// One `T_PlatRaise`.
    pub fn tic(&mut self) -> Step {
        match self.status {
            DOWN => {
                if self.floorheight - self.speed < self.low {
                    self.floorheight = self.low;
                    self.count = self.wait;
                    self.status = WAITING;
                    self.reached_bottom = true;
                } else {
                    self.floorheight -= self.speed;
                }
            }
            UP => {
                if self.floorheight + self.speed > self.high {
                    self.floorheight = self.high;
                    self.count = self.wait;
                    self.status = WAITING;
                } else {
                    self.floorheight += self.speed;
                }
            }
            WAITING => {
                self.count -= 1;
                if self.count == 0 {
                    self.status = if self.floorheight == self.low {
                        UP
                    } else {
                        DOWN
                    };
                    self.waited = true;
                }
            }
            _ => {}
        }
        Step {
            floorheight: self.floorheight,
            status: self.status,
            count: self.count,
        }
    }
}

/// The overrides that put a `downWaitUpStay` plat on the end of the
/// thinker list with the sector pointed at it.
pub fn seed(db: &str, tic: u32, sector: usize, high: i32, low: i32) -> Vec<String> {
    let appended: Vec<(&str, String)> = vec![
        ("s_seq", "toUInt32(p.next_seq)".to_owned()),
        ("s_kind", format!("toUInt8({})", sector_thinker_kind::PLAT)),
        ("s_sector", format!("toInt32({sector})")),
        ("s_type", format!("toInt32({DOWN_WAIT_UP_STAY})")),
        ("s_direction", "toInt32(0)".to_owned()),
        ("s_speed", format!("toInt32({})", PLATSPEED * 4)),
        ("s_dest", format!("toInt32({low})")),
        ("s_dest2", format!("toInt32({high})")),
        ("s_count", "toInt32(0)".to_owned()),
        ("s_wait", format!("toInt32({})", TICRATE * PLATWAIT)),
        ("s_status", format!("toInt32({DOWN})")),
        ("s_active", "toUInt8(1)".to_owned()),
    ];
    let mut overrides: Vec<(&str, String)> = vec![(
        "sec_specialdata",
        format!(
            "arrayMap((v, i) -> toUInt32(if(i = {}, length(p.s_kind) + 1, v)), \
             p.sec_specialdata, arrayEnumerate(p.sec_specialdata))",
            sector + 1
        ),
    )];
    for column in sim::state_columns() {
        if !column.starts_with("s_") {
            continue;
        }
        let value = match appended.iter().find(|(name, _)| *name == column) {
            Some((_, value)) => value.clone(),
            None => format!("p.{column}[1]"),
        };
        overrides.push((column, format!("arrayPushBack(p.{column}, {value})")));
    }
    super::seed::row(db, tic, 1, &overrides)
}
