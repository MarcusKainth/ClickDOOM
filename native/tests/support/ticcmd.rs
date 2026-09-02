//! `G_BuildTiccmd` over a key word and two mouse deltas, read a second
//! time.
//!
//! This is an oracle and nothing else. Native mode builds the command in
//! SQL; this follows `g_game.c` instead, so the two agreeing means
//! something.

use clickdoom_spec::native_state::key;

/// `g_game.c`
const FORWARDMOVE: [i32; 2] = [0x19, 0x32];
const SIDEMOVE: [i32; 2] = [0x18, 0x28];
const ANGLETURN: [i32; 3] = [640, 1280, 320];
const SLOWTURNTICS: i32 = 6;
const MAXPLMOVE: i32 = FORWARDMOVE[1];
/// `d_loop.c`
const TICDUP: i32 = 1;

/// `d_event.h`
const BT_ATTACK: i32 = 1;
const BT_USE: i32 = 2;
const BT_SPECIAL: i32 = 128;
const BT_CHANGE: i32 = 4;
const BT_WEAPONSHIFT: i32 = 3;
const BTS_PAUSE: i32 = 1;

/// One tic's command.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ticcmd {
    pub forwardmove: i8,
    pub sidemove: i8,
    pub angleturn: i16,
    pub buttons: u8,
}

/// How long a turn key has been held, which is the only thing
/// `G_BuildTiccmd` carries between tics.
#[derive(Default)]
pub struct Input {
    pub turnheld: i32,
}

impl Input {
    /// One tic of `G_BuildTiccmd`, without the joystick, the mouse buttons
    /// or the double-click gestures, none of which the key word carries.
    pub fn build(&mut self, keys: u32, mouse: (i16, i16)) -> Ticcmd {
        let (mousex, mousey) = (i32::from(mouse.0), i32::from(mouse.1));
        let down = |bit: u32| keys & bit != 0;
        let strafe = down(key::STRAFE);
        let speed = usize::from(down(key::SPEED));

        if down(key::RIGHT) || down(key::LEFT) {
            self.turnheld += TICDUP;
        } else {
            self.turnheld = 0;
        }
        let tspeed = if self.turnheld < SLOWTURNTICS {
            2
        } else {
            speed
        };

        let mut cmd = Ticcmd::default();
        let mut forward = 0;
        let mut side = 0;
        let mut angleturn = 0;
        if strafe {
            if down(key::RIGHT) {
                side += SIDEMOVE[speed];
            }
            if down(key::LEFT) {
                side -= SIDEMOVE[speed];
            }
        } else {
            if down(key::RIGHT) {
                angleturn -= ANGLETURN[tspeed];
            }
            if down(key::LEFT) {
                angleturn += ANGLETURN[tspeed];
            }
        }
        if down(key::UP) {
            forward += FORWARDMOVE[speed];
        }
        if down(key::DOWN) {
            forward -= FORWARDMOVE[speed];
        }
        if down(key::STRAFE_LEFT) {
            side -= SIDEMOVE[speed];
        }
        if down(key::STRAFE_RIGHT) {
            side += SIDEMOVE[speed];
        }

        let mut buttons = 0;
        if down(key::FIRE) {
            buttons |= BT_ATTACK;
        }
        if down(key::USE) {
            buttons |= BT_USE;
        }
        let weapons = (keys & key::WEAPON_MASK) >> key::WEAPON_SHIFT;
        if weapons != 0 {
            buttons |= BT_CHANGE;
            buttons |= (weapons.trailing_zeros() as i32) << BT_WEAPONSHIFT;
        }

        forward += mousey;
        if strafe {
            side += mousex * 2;
        } else {
            angleturn -= mousex * 8;
        }

        if down(key::PAUSE) {
            buttons = BT_SPECIAL | BTS_PAUSE;
        }

        cmd.forwardmove = forward.clamp(-MAXPLMOVE, MAXPLMOVE) as i8;
        cmd.sidemove = side.clamp(-MAXPLMOVE, MAXPLMOVE) as i8;
        cmd.angleturn = angleturn as i16;
        cmd.buttons = buttons as u8;
        cmd
    }
}
