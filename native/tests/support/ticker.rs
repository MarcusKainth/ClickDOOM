//! `ST_updateFaceWidget` and `P_InitPicAnims`, read a second time.
//!
//! This is an oracle and nothing else. Native mode runs the tickers in SQL;
//! this follows `st_stuff.c` and `p_spec.c` instead, so the two agreeing
//! means something.

use clickdoom_native::tables;

/// What the player carries through a run that neither hurts them nor gives
/// them anything: the face reads it and nothing moves it.
pub const PLAYER_HEALTH: i32 = 100;

/// `st_stuff.c`
const NUMPAINFACES: i32 = 5;
const NUMSTRAIGHTFACES: i32 = 3;
const NUMTURNFACES: i32 = 2;
const NUMSPECIALFACES: i32 = 3;
const FACESTRIDE: i32 = NUMSTRAIGHTFACES + NUMTURNFACES + NUMSPECIALFACES;
const TURNOFFSET: i32 = NUMSTRAIGHTFACES;
const OUCHOFFSET: i32 = TURNOFFSET + NUMTURNFACES;
const EVILGRINOFFSET: i32 = OUCHOFFSET + 1;
const RAMPAGEOFFSET: i32 = EVILGRINOFFSET + 1;
const TICRATE: i32 = 35;
const STRAIGHTFACECOUNT: i32 = TICRATE / 2;
const RAMPAGEDELAY: i32 = 2 * TICRATE;

/// The status bar's face, and the statics that decide it.
pub struct Face {
    pub faceindex: i32,
    pub facecount: i32,
    pub priority: i32,
    pub lastattackdown: i32,
    pub lastcalc: i32,
    pub calc_oldhealth: i32,
}

impl Default for Face {
    /// What `ST_initData` and the file statics leave behind.
    fn default() -> Face {
        Face {
            faceindex: 0,
            facecount: 0,
            priority: 0,
            lastattackdown: -1,
            lastcalc: 0,
            calc_oldhealth: -1,
        }
    }
}

impl Face {
    /// `ST_calcPainOffset`, which caches its answer against the health it
    /// last saw.
    fn pain_offset(&mut self) -> i32 {
        let health = PLAYER_HEALTH.min(100);
        if health != self.calc_oldhealth {
            self.lastcalc = FACESTRIDE * ((100 - health) * NUMPAINFACES / 101);
            self.calc_oldhealth = health;
        }
        self.lastcalc
    }

    /// One tic of `ST_updateFaceWidget`, for a player nothing has hurt.
    ///
    /// `attackdown` is the player's own, which `A_WeaponReady` clears on
    /// the first tic the weapon is up and the attack button is not down.
    pub fn update(&mut self, randomnumber: i32, attackdown: bool) {
        // The dead, evil grin, attacked and hurt rungs need a health of
        // zero, a pickup or damage, and this run has none of them.
        if self.priority < 6 {
            if attackdown {
                if self.lastattackdown == -1 {
                    self.lastattackdown = RAMPAGEDELAY;
                } else {
                    self.lastattackdown -= 1;
                    if self.lastattackdown == 0 {
                        self.priority = 5;
                        self.faceindex = self.pain_offset() + RAMPAGEOFFSET;
                        self.facecount = 1;
                        self.lastattackdown = 1;
                    }
                }
            } else {
                self.lastattackdown = -1;
            }
        }
        if self.facecount == 0 {
            self.faceindex = self.pain_offset() + randomnumber % 3;
            self.facecount = STRAIGHTFACECOUNT;
            self.priority = 0;
        }
        self.facecount -= 1;
    }
}

/// One entry of `P_InitPicAnims`' table.
pub struct Anim {
    pub istexture: bool,
    pub basepic: i32,
    pub numpics: i32,
    pub speed: i32,
}

/// `P_InitPicAnims`: every cycle whose first picture this WAD carries.
pub fn anims(textures: &[String], flats: &[String]) -> Vec<Anim> {
    let table = tables::table("animdefs").unwrap();
    let istexture = table.ints("istexture").unwrap();
    let speed = table.ints("speed").unwrap();
    let startname = table.texts("startname").unwrap();
    let endname = table.texts("endname").unwrap();
    let number = |names: &[String], name: &str| {
        names
            .iter()
            .position(|known| known.eq_ignore_ascii_case(name))
            .map(|at| at as i32)
    };
    let mut anims = Vec::new();
    for at in 0..istexture.len() {
        if istexture[at] == -1 {
            break;
        }
        let names = if istexture[at] != 0 { textures } else { flats };
        let Some(basepic) = number(names, startname[at]) else {
            continue;
        };
        let picnum = number(names, endname[at]).expect("a cycle that starts also ends");
        anims.push(Anim {
            istexture: istexture[at] != 0,
            basepic,
            numpics: picnum - basepic + 1,
            speed: speed[at] as i32,
        });
    }
    anims
}

/// One translation table as `P_UpdateSpecials` leaves it at `leveltime`.
pub fn translation(anims: &[Anim], istexture: bool, count: usize, leveltime: i32) -> Vec<i32> {
    let mut table: Vec<i32> = (0..count as i32).collect();
    for anim in anims.iter().filter(|anim| anim.istexture == istexture) {
        for i in anim.basepic..anim.basepic + anim.numpics {
            table[i as usize] = anim.basepic + (leveltime / anim.speed + i) % anim.numpics;
        }
    }
    table
}
