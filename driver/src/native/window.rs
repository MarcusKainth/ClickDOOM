//! The window a paced run blits into.
//!
//! The bytes are the ones SQL produced: `rgb32` is 256,000 bytes of
//! little-endian words, one per pixel, and the window takes words. The only
//! thing that happens between the table and the screen is reading four
//! bytes as the word they are.
//!
//! Behind the `window` feature. Without it the type is still here and
//! [`Window::open`] still refuses, so a headless build has the same command
//! line with `--no-window`.

/// The frame the renderer draws, as `NATIVE.md` fixes it.
pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 200;

/// Bytes of `rgb32`, four per pixel.
pub const RGB32_BYTES: usize = WIDTH * HEIGHT * 4;

/// Anything that stops a frame from reaching the screen.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("opening a window: {0}. Run with --no-window for a headless run")]
    Open(String),
    #[error("drawing into the window: {0}")]
    Draw(String),
    #[error(
        "a frame is {found} bytes, not the {RGB32_BYTES} a {WIDTH}x{HEIGHT} \
         frame has"
    )]
    FrameSize { found: usize },
    #[error(
        "this binary was built without the `window` feature. Run with \
         --no-window, or rebuild with it"
    )]
    NotBuilt,
}

/// Which key on the keyboard sends which of the engine's key bits.
///
/// A binding, not a computation: the bits are
/// `clickdoom_spec::native_state::key`, and the SQL side is what builds a
/// tic command out of them.
#[cfg(feature = "window")]
const BINDINGS: [(minifb::Key, u32); 18] = {
    use clickdoom_spec::native_state::key;
    use minifb::Key;
    [
        (Key::Right, key::RIGHT),
        (Key::Left, key::LEFT),
        (Key::Up, key::UP),
        (Key::W, key::UP),
        (Key::Down, key::DOWN),
        (Key::S, key::DOWN),
        (Key::LeftCtrl, key::FIRE),
        (Key::RightCtrl, key::FIRE),
        (Key::Space, key::USE),
        (Key::E, key::USE),
        (Key::LeftAlt, key::STRAFE),
        (Key::RightAlt, key::STRAFE),
        (Key::LeftShift, key::SPEED),
        (Key::RightShift, key::SPEED),
        (Key::Comma, key::STRAFE_LEFT),
        (Key::A, key::STRAFE_LEFT),
        (Key::Period, key::STRAFE_RIGHT),
        (Key::D, key::STRAFE_RIGHT),
    ]
};

/// The weapon keys, in the order the engine numbers the weapons.
#[cfg(feature = "window")]
const WEAPONS: [minifb::Key; 7] = {
    use minifb::Key;
    [
        Key::Key1,
        Key::Key2,
        Key::Key3,
        Key::Key4,
        Key::Key5,
        Key::Key6,
        Key::Key7,
    ]
};

/// How much bigger than 320x200 the window is drawn.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Scale {
    #[value(name = "1")]
    One,
    #[value(name = "2")]
    Two,
    #[value(name = "4")]
    Four,
}

/// Reads `rgb32` into `buffer` as the words it holds.
///
/// The bytes come out of the table untouched; a word is four of them, least
/// significant first, which is how SQL wrote them.
///
/// Public because it is the whole of what happens between the table and the
/// screen, and a test that has a frame in hand can check the two ends
/// against each other without opening a window.
pub fn words(rgb32: &[u8], buffer: &mut Vec<u32>) -> Result<(), Error> {
    if rgb32.len() != RGB32_BYTES {
        return Err(Error::FrameSize { found: rgb32.len() });
    }
    buffer.clear();
    buffer.extend(
        rgb32
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(u32::from_le_bytes),
    );
    Ok(())
}

#[cfg(feature = "window")]
mod backend {
    use super::{BINDINGS, Error, HEIGHT, Scale, WEAPONS, WIDTH, words};

    /// One open window, and the buffer a frame is read into.
    pub struct Window {
        window: minifb::Window,
        buffer: Vec<u32>,
        /// Where the pointer was when the last tic sampled it, once there
        /// has been a tic to sample it from. A window that has not been
        /// drawn to has not pumped its events, and the position it reports
        /// is not one the pointer was ever at.
        pointer: Option<(f32, f32)>,
        /// Whether the window has drawn a frame, and so pumped its events.
        drawn: bool,
        /// Whether the pause key was down when the last tic sampled it.
        /// The engine takes a pause as one press, not as a key being held.
        paused_held: bool,
    }

    impl Window {
        /// Opens the window. The run paces itself, so the window's own rate
        /// limiter is off.
        pub fn open(title: &str, scale: Scale) -> Result<Window, Error> {
            let options = minifb::WindowOptions {
                scale: match scale {
                    Scale::One => minifb::Scale::X1,
                    Scale::Two => minifb::Scale::X2,
                    Scale::Four => minifb::Scale::X4,
                },
                ..Default::default()
            };
            let mut window = minifb::Window::new(title, WIDTH, HEIGHT, options)
                .map_err(|e| Error::Open(e.to_string()))?;
            window.set_target_fps(0);
            Ok(Window {
                window,
                buffer: Vec::with_capacity(WIDTH * HEIGHT),
                pointer: None,
                drawn: false,
                paused_held: false,
            })
        }

        /// Puts one frame on the screen and pumps the window's events.
        pub fn draw(&mut self, rgb32: &[u8]) -> Result<(), Error> {
            words(rgb32, &mut self.buffer)?;
            self.window
                .update_with_buffer(&self.buffer, WIDTH, HEIGHT)
                .map_err(|e| Error::Draw(e.to_string()))?;
            self.drawn = true;
            Ok(())
        }

        /// Whether the window is still there. A run stops when it is not.
        pub fn is_open(&self) -> bool {
            self.window.is_open() && !self.window.is_key_down(minifb::Key::Escape)
        }

        /// The key bits down now, ferried through unchanged.
        ///
        /// The pause bit is set for the one sample the key goes down on,
        /// because the engine takes it as a press rather than as a hold and
        /// would otherwise toggle every tic it is held for.
        pub fn keys(&mut self) -> u32 {
            use clickdoom_spec::native_state::key;
            let mut bits = 0;
            for (from, bit) in BINDINGS {
                if self.window.is_key_down(from) {
                    bits |= bit;
                }
            }
            if self.window.get_mouse_down(minifb::MouseButton::Left) {
                bits |= key::FIRE;
            }
            if self.window.get_mouse_down(minifb::MouseButton::Right) {
                bits |= key::USE;
            }
            for (weapon, from) in WEAPONS.into_iter().enumerate() {
                if self.window.is_key_down(from) {
                    bits |= (weapon as u32 + 1) << key::WEAPON_SHIFT;
                    break;
                }
            }
            let pause = self.window.is_key_down(minifb::Key::P);
            if pause && !self.paused_held {
                bits |= key::PAUSE;
            }
            self.paused_held = pause;
            bits
        }

        /// How far the pointer moved since the last sample.
        ///
        /// Nothing is reported until a frame has been drawn. A window that
        /// has not been drawn to has not pumped its events, and taking that
        /// position as an origin turns the first real reading into a jump
        /// the pointer never made.
        ///
        /// The pointer is not captured, so it stops at the edge of the
        /// window and the movement stops with it.
        pub fn mouse(&mut self) -> (i16, i16) {
            if !self.drawn {
                return (0, 0);
            }
            let Some(at) = self.window.get_mouse_pos(minifb::MouseMode::Pass) else {
                return (0, 0);
            };
            let moved = match self.pointer {
                Some((x, y)) => ((at.0 - x) as i16, (at.1 - y) as i16),
                None => (0, 0),
            };
            self.pointer = Some(at);
            moved
        }
    }
}

#[cfg(not(feature = "window"))]
mod backend {
    use super::{Error, Scale};

    /// The window this build does not have.
    pub struct Window {}

    impl Window {
        pub fn open(_title: &str, _scale: Scale) -> Result<Window, Error> {
            Err(Error::NotBuilt)
        }

        pub fn draw(&mut self, _rgb32: &[u8]) -> Result<(), Error> {
            Err(Error::NotBuilt)
        }

        pub fn is_open(&self) -> bool {
            false
        }

        pub fn keys(&mut self) -> u32 {
            0
        }

        pub fn mouse(&mut self) -> (i16, i16) {
            (0, 0)
        }
    }
}

pub use backend::Window;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_four_bytes_least_significant_first() {
        let mut rgb32 = vec![0u8; RGB32_BYTES];
        rgb32[..8].copy_from_slice(&[0x11, 0x22, 0x33, 0x00, 0xff, 0xee, 0xdd, 0x00]);
        let mut buffer = Vec::new();
        words(&rgb32, &mut buffer).expect("a whole frame");
        assert_eq!(buffer.len(), WIDTH * HEIGHT);
        assert_eq!(buffer[0], 0x0033_2211);
        assert_eq!(buffer[1], 0x00dd_eeff);
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_refused_rather_than_drawn_short() {
        let mut buffer = Vec::new();
        let error = words(&[0; 12], &mut buffer).expect_err("not a frame");
        assert!(matches!(error, Error::FrameSize { found: 12 }), "{error}");
    }

    /// Every binding names a bit the contract declares, and the weapon keys
    /// sit where it says they sit. A binding onto a bit nothing reads is a
    /// key that does nothing and says nothing about it.
    #[cfg(feature = "window")]
    #[test]
    fn every_binding_names_a_key_bit_the_contract_declares() {
        use clickdoom_spec::native_state::key;
        let declared = key::RIGHT
            | key::LEFT
            | key::UP
            | key::DOWN
            | key::FIRE
            | key::USE
            | key::STRAFE
            | key::SPEED
            | key::STRAFE_LEFT
            | key::STRAFE_RIGHT
            | key::PAUSE;
        for (from, bit) in BINDINGS {
            assert_eq!(bit.count_ones(), 1, "{from:?} sends more than one bit");
            assert_eq!(bit & declared, bit, "{from:?} sends a bit nothing declares");
        }
        for (weapon, from) in WEAPONS.into_iter().enumerate() {
            let bits = (weapon as u32 + 1) << key::WEAPON_SHIFT;
            assert_eq!(
                bits & key::WEAPON_MASK,
                bits,
                "{from:?} is not a weapon key"
            );
        }
    }

    /// The buffer is reused between frames, so it has to be emptied first.
    #[test]
    fn a_second_frame_replaces_the_first_rather_than_growing_the_buffer() {
        let mut buffer = Vec::new();
        words(&vec![0u8; RGB32_BYTES], &mut buffer).expect("a frame");
        words(&vec![1u8; RGB32_BYTES], &mut buffer).expect("another frame");
        assert_eq!(buffer.len(), WIDTH * HEIGHT);
        assert_eq!(buffer[0], 0x0101_0101);
    }
}
