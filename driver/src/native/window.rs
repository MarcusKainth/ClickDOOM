//! The window a paced run blits into.
//!
//! The bytes are the ones SQL produced: `rgb32` is 256,000 bytes of
//! little-endian words, one per pixel, and the window takes words. Between
//! the table and the screen a word is read out of its four bytes and
//! repeated to fill the whole number of screen pixels it is drawn at.
//! Nothing is blended.
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
/// tic command out of them. A key is named by its place on the keyboard
/// rather than by the character a layout puts there, so WASD stays under
/// the same fingers on a layout that is not QWERTY.
#[cfg(feature = "window")]
const BINDINGS: [(winit::keyboard::KeyCode, u32); 18] = {
    use clickdoom_spec::native_state::key;
    use winit::keyboard::KeyCode;
    [
        (KeyCode::ArrowRight, key::RIGHT),
        (KeyCode::ArrowLeft, key::LEFT),
        (KeyCode::ArrowUp, key::UP),
        (KeyCode::KeyW, key::UP),
        (KeyCode::ArrowDown, key::DOWN),
        (KeyCode::KeyS, key::DOWN),
        (KeyCode::ControlLeft, key::FIRE),
        (KeyCode::ControlRight, key::FIRE),
        (KeyCode::Space, key::USE),
        (KeyCode::KeyE, key::USE),
        (KeyCode::AltLeft, key::STRAFE),
        (KeyCode::AltRight, key::STRAFE),
        (KeyCode::ShiftLeft, key::SPEED),
        (KeyCode::ShiftRight, key::SPEED),
        (KeyCode::Comma, key::STRAFE_LEFT),
        (KeyCode::KeyA, key::STRAFE_LEFT),
        (KeyCode::Period, key::STRAFE_RIGHT),
        (KeyCode::KeyD, key::STRAFE_RIGHT),
    ]
};

/// The weapon keys, in the order the engine numbers the weapons.
#[cfg(feature = "window")]
const WEAPONS: [winit::keyboard::KeyCode; 7] = {
    use winit::keyboard::KeyCode;
    [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
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

#[cfg(feature = "window")]
impl Scale {
    /// The window's size in screen points, as a multiple of the frame's.
    fn factor(self) -> f64 {
        match self {
            Scale::One => 1.0,
            Scale::Two => 2.0,
            Scale::Four => 4.0,
        }
    }
}

/// Where the run's cursor is, and whether the run goes on.
///
/// A run starts with the cursor free. A click inside the window takes it,
/// Escape gives it back, and Escape with the cursor already free ends the
/// run. Losing focus gives it back too, and regaining focus does not take
/// it again, so a window that comes back under the pointer does not seize
/// it.
#[cfg(feature = "window")]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum Grab {
    #[default]
    Released,
    Locked,
    Closing,
}

#[cfg(feature = "window")]
impl Grab {
    /// Escape, once per press.
    fn escaped(self) -> Grab {
        match self {
            Grab::Locked => Grab::Released,
            Grab::Released | Grab::Closing => Grab::Closing,
        }
    }

    /// A mouse button pressed inside the window.
    fn clicked(self) -> Grab {
        match self {
            Grab::Released => Grab::Locked,
            other => other,
        }
    }

    /// The window losing keyboard focus.
    fn unfocused(self) -> Grab {
        match self {
            Grab::Locked => Grab::Released,
            other => other,
        }
    }

    /// Whether the cursor is held and hidden.
    fn holds_cursor(self) -> bool {
        matches!(self, Grab::Locked)
    }

    /// Whether the run goes on.
    fn open(self) -> bool {
        !matches!(self, Grab::Closing)
    }
}

/// The mouse motion that has arrived since the last tic sampled it.
///
/// The window system reports motion in fractions of a unit. What a sample
/// cannot carry as a whole number stays here for the next one, so slow
/// movement is not rounded away.
#[cfg(feature = "window")]
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct Motion {
    x: f64,
    y: f64,
}

#[cfg(feature = "window")]
impl Motion {
    /// Adds one report of relative motion.
    fn add(&mut self, (x, y): (f64, f64)) {
        self.x += x;
        self.y += y;
    }

    /// Takes the whole part of what has accumulated, leaving the rest.
    fn drain(&mut self) -> (i16, i16) {
        let (x, dx) = Motion::split(self.x);
        let (y, dy) = Motion::split(self.y);
        self.x = x;
        self.y = y;
        (dx, dy)
    }

    /// Forgets everything that has accumulated.
    fn clear(&mut self) {
        *self = Motion::default();
    }

    /// Splits `moved` into what stays behind and what one sample takes. A
    /// sample takes at most what an input row's `i16` carries.
    fn split(moved: f64) -> (f64, i16) {
        let taken = moved
            .trunc()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX));
        (moved - taken, taken as i16)
    }
}

/// Everything the event loop has told the run since the last tic sampled it.
///
/// The window system hands events over one at a time; a tic reads the state
/// they left behind. Nothing here touches a window, so what a sequence of
/// events does to a sample is checkable on its own.
#[cfg(feature = "window")]
#[derive(Default)]
struct Run {
    grab: Grab,
    /// The keys held down, by their place on the keyboard.
    held: std::collections::HashSet<winit::keyboard::KeyCode>,
    left: bool,
    right: bool,
    /// Set when the pause key goes down and cleared by the sample that
    /// reads it. The engine takes a pause as one press, not as a key being
    /// held.
    paused: bool,
    motion: Motion,
}

#[cfg(feature = "window")]
impl Run {
    /// Whether the run holds the cursor, and so reads the mouse.
    fn holds_cursor(&self) -> bool {
        self.grab.holds_cursor()
    }

    /// Whether the run goes on.
    fn open(&self) -> bool {
        self.grab.open()
    }

    /// Moves the cursor state on. Giving the cursor back drops the motion
    /// that came with it, so taking it again does not turn the player by
    /// everything the pointer did in between.
    fn to(&mut self, next: Grab) {
        if self.grab.holds_cursor() && !next.holds_cursor() {
            self.motion.clear();
        }
        self.grab = next;
    }

    /// One key going down. Escape and the pause key are presses rather than
    /// holds, so neither joins the held set.
    fn key_down(&mut self, code: winit::keyboard::KeyCode) {
        use winit::keyboard::KeyCode;
        match code {
            KeyCode::Escape => self.to(self.grab.escaped()),
            KeyCode::KeyP => self.paused = true,
            _ => {
                self.held.insert(code);
            }
        }
    }

    /// One key coming up.
    fn key_up(&mut self, code: winit::keyboard::KeyCode) {
        self.held.remove(&code);
    }

    /// One mouse button going down or coming up.
    ///
    /// The press that takes the cursor goes no further, so clicking back
    /// into the window does not also fire.
    fn button(&mut self, down: bool, button: winit::event::MouseButton) {
        use winit::event::MouseButton;
        if down && !self.holds_cursor() {
            self.to(self.grab.clicked());
            return;
        }
        match button {
            MouseButton::Left => self.left = down,
            MouseButton::Right => self.right = down,
            _ => {}
        }
    }

    /// The window losing keyboard focus. No key comes up while the window
    /// is not listening, so none stays down either.
    fn unfocused(&mut self) {
        self.held.clear();
        self.left = false;
        self.right = false;
        self.paused = false;
        self.to(self.grab.unfocused());
    }

    /// One report of relative mouse motion, kept only while the run holds
    /// the cursor.
    fn moved(&mut self, delta: (f64, f64)) {
        if self.holds_cursor() {
            self.motion.add(delta);
        }
    }

    /// The key bits down now, ferried through unchanged.
    fn keys(&mut self) -> u32 {
        use clickdoom_spec::native_state::key;
        let mut bits = 0;
        for (code, bit) in BINDINGS {
            if self.held.contains(&code) {
                bits |= bit;
            }
        }
        if self.holds_cursor() {
            if self.left {
                bits |= key::FIRE;
            }
            if self.right {
                bits |= key::USE;
            }
        }
        for (weapon, code) in WEAPONS.into_iter().enumerate() {
            if self.held.contains(&code) {
                bits |= (weapon as u32 + 1) << key::WEAPON_SHIFT;
                break;
            }
        }
        if std::mem::take(&mut self.paused) {
            bits |= key::PAUSE;
        }
        bits
    }

    /// How far the mouse moved since the last sample.
    fn mouse(&mut self) -> (i16, i16) {
        self.motion.drain()
    }
}

/// Reads `rgb32` into `buffer` as the words it holds.
///
/// The bytes come out of the table untouched; a word is four of them, least
/// significant first, which is how SQL wrote them.
///
/// Public because it is one of the two things that happen between the table
/// and the screen, and a test that has a frame in hand can check the two
/// ends against each other without opening a window.
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

/// Draws `frame` into a `width` by `height` screen buffer, at the largest
/// whole number of screen pixels per frame pixel that fits, centred, with
/// the rest black.
///
/// Every word written is a word `frame` holds. A frame pixel is repeated,
/// never blended, so what reaches the screen is what the renderer produced.
#[cfg(feature = "window")]
fn blit(frame: &[u32], screen: &mut [u32], width: usize, height: usize) {
    let scale = (width / WIDTH).min(height / HEIGHT);
    if scale == 0 || frame.len() < WIDTH * HEIGHT || screen.len() < width * height {
        screen.fill(0);
        return;
    }
    let (drawn_width, drawn_height) = (WIDTH * scale, HEIGHT * scale);
    if (drawn_width, drawn_height) != (width, height) {
        screen.fill(0);
    }
    let left = (width - drawn_width) / 2;
    let top = (height - drawn_height) / 2;
    for row in 0..HEIGHT {
        let source = &frame[row * WIDTH..][..WIDTH];
        let at = (top + row * scale) * width + left;
        let drawn = &mut screen[at..][..drawn_width];
        if scale == 1 {
            drawn.copy_from_slice(source);
        } else {
            for (word, run) in source.iter().zip(drawn.chunks_exact_mut(scale)) {
                run.fill(*word);
            }
        }
        for line in 1..scale {
            let (above, below) = screen.split_at_mut(at + line * width);
            below[..drawn_width].copy_from_slice(&above[at..][..drawn_width]);
        }
    }
}

#[cfg(feature = "window")]
mod backend {
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::time::Duration; // purity-ok: the event pump's timeout, a constant zero, read from no clock

    use winit::application::ApplicationHandler;
    use winit::dpi::LogicalSize;
    use winit::event::{DeviceEvent, DeviceId, ElementState, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::keyboard::PhysicalKey;
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
    use winit::window::{CursorGrabMode, WindowAttributes, WindowId};

    use super::{Error, Grab, HEIGHT, Run, Scale, WIDTH, blit, words};

    /// The words on their way to the window, and the display they go through.
    type Surface = softbuffer::Surface<Arc<winit::window::Window>, Arc<winit::window::Window>>;

    /// The window, and the run the event loop writes into.
    ///
    /// `winit` hands events to this rather than returning them. Each one is
    /// turned into a call on [`Run`], and the cursor is made to match
    /// whatever that leaves behind.
    struct App {
        title: String,
        scale: Scale,
        window: Option<Arc<winit::window::Window>>,
        surface: Option<Surface>,
        /// The screen buffer's size, so it is resized only when it changes.
        size: Option<(NonZeroU32, NonZeroU32)>,
        /// Why the window could not be opened, for [`Window::open`] to
        /// report. The event loop has no other way back to its caller.
        refused: Option<Error>,
        /// Whether the run has already said the cursor cannot be held.
        warned: bool,
        run: Run,
    }

    impl App {
        fn new(title: &str, scale: Scale) -> App {
            App {
                title: title.to_owned(),
                scale,
                window: None,
                surface: None,
                size: None,
                refused: None,
                warned: false,
                run: Run::default(),
            }
        }

        /// A window `--scale` times the frame's size, in screen points.
        fn attributes(&self) -> WindowAttributes {
            let factor = self.scale.factor();
            winit::window::Window::default_attributes()
                .with_title(self.title.as_str())
                .with_inner_size(LogicalSize::new(
                    WIDTH as f64 * factor,
                    HEIGHT as f64 * factor,
                ))
                .with_resizable(false)
        }

        /// Makes the cursor match the run after an event. `before` is
        /// whether the run held the cursor when that event arrived.
        fn follow(&mut self, before: bool) {
            let now = self.run.holds_cursor();
            if now != before {
                self.hold_cursor(now);
            }
        }

        /// Holds the cursor and hides it, or gives it back.
        ///
        /// `Locked` freezes the cursor in place, and macOS and Wayland
        /// implement it. X11 implements `Confined` instead, which keeps the
        /// cursor inside the window. Under either the motion a sample reads
        /// comes from the device rather than from where the cursor is.
        fn hold_cursor(&mut self, hold: bool) {
            let Some(window) = self.window.clone() else {
                return;
            };
            window.set_cursor_visible(!hold);
            let grabbed = if hold {
                window
                    .set_cursor_grab(CursorGrabMode::Locked)
                    .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
            } else {
                window.set_cursor_grab(CursorGrabMode::None)
            };
            if let Err(err) = grabbed
                && !self.warned
            {
                eprintln!("clickdoom: the window system will not hold the cursor: {err}");
                self.warned = true;
            }
        }

        /// Puts `frame` on the screen, at the size the window is now.
        fn present(&mut self, frame: &[u32]) -> Result<(), Error> {
            let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
                return Ok(());
            };
            let inner = window.inner_size();
            let (Some(width), Some(height)) =
                (NonZeroU32::new(inner.width), NonZeroU32::new(inner.height))
            else {
                return Ok(());
            };
            if self.size != Some((width, height)) {
                surface
                    .resize(width, height)
                    .map_err(|err| Error::Draw(err.to_string()))?;
                self.size = Some((width, height));
            }
            let mut screen = surface
                .buffer_mut()
                .map_err(|err| Error::Draw(err.to_string()))?;
            blit(
                frame,
                &mut screen,
                width.get() as usize,
                height.get() as usize,
            );
            screen.present().map_err(|err| Error::Draw(err.to_string()))
        }
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.window.is_some() || self.refused.is_some() {
                return;
            }
            let window = match event_loop.create_window(self.attributes()) {
                Ok(window) => Arc::new(window),
                Err(err) => {
                    self.refused = Some(Error::Open(err.to_string()));
                    return;
                }
            };
            // The context is only needed to make the surface, which keeps
            // whatever it needs of the display.
            let made = softbuffer::Context::new(window.clone())
                .and_then(|context| Surface::new(&context, window.clone()));
            match made {
                Ok(surface) => {
                    self.window = Some(window);
                    self.surface = Some(surface);
                }
                Err(err) => self.refused = Some(Error::Open(err.to_string())),
            }
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            let held = self.run.holds_cursor();
            match event {
                WindowEvent::CloseRequested | WindowEvent::Destroyed => self.run.to(Grab::Closing),
                WindowEvent::Focused(false) => self.run.unfocused(),
                WindowEvent::KeyboardInput { event, .. } => {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        match event.state {
                            // A key held down repeats, and a repeat is not a
                            // press.
                            ElementState::Pressed if event.repeat => {}
                            ElementState::Pressed => self.run.key_down(code),
                            ElementState::Released => self.run.key_up(code),
                        }
                    }
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    self.run.button(state == ElementState::Pressed, button);
                }
                _ => {}
            }
            self.follow(held);
        }

        fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
            if let DeviceEvent::MouseMotion { delta } = event {
                self.run.moved(delta);
            }
        }
    }

    /// One open window, and the buffer a frame is read into.
    pub struct Window {
        events: EventLoop<()>,
        app: App,
        buffer: Vec<u32>,
    }

    impl Window {
        /// Opens the window. The run paces itself, so the event loop never
        /// waits: a pump takes whatever has arrived and returns.
        pub fn open(title: &str, scale: Scale) -> Result<Window, Error> {
            let mut events = EventLoop::new().map_err(|err| Error::Open(err.to_string()))?;
            let mut app = App::new(title, scale);
            // The first pump starts the event loop, which resumes the
            // application, which is where the window is made.
            let status = events.pump_app_events(Some(Duration::ZERO), &mut app);
            if let Some(err) = app.refused.take() {
                return Err(err);
            }
            if let PumpStatus::Exit(code) = status {
                return Err(Error::Open(format!("the event loop stopped with {code}")));
            }
            if app.window.is_none() {
                return Err(Error::Open(
                    "the event loop did not resume, so there is no window".to_owned(),
                ));
            }
            Ok(Window {
                events,
                app,
                buffer: Vec::with_capacity(WIDTH * HEIGHT),
            })
        }

        /// Puts one frame on the screen and takes in everything the window
        /// system has queued since the last frame.
        ///
        /// The keys and the motion a tic samples are what this left behind,
        /// so a run that stops drawing stops reading input.
        pub fn draw(&mut self, rgb32: &[u8]) -> Result<(), Error> {
            words(rgb32, &mut self.buffer)?;
            let status = self
                .events
                .pump_app_events(Some(Duration::ZERO), &mut self.app);
            if let PumpStatus::Exit(_) = status {
                self.app.run.to(Grab::Closing);
            }
            self.app.present(&self.buffer)
        }

        /// Whether the run goes on. It stops when the window is closed, and
        /// on Escape with the cursor already free.
        pub fn is_open(&self) -> bool {
            self.app.run.open()
        }

        /// The key bits down now, ferried through unchanged.
        ///
        /// The pause bit is set for the one sample after the key goes down,
        /// because the engine takes it as a press rather than as a hold and
        /// would otherwise toggle every tic it is held for. The mouse
        /// buttons count only while the run holds the cursor.
        pub fn keys(&mut self) -> u32 {
            self.app.run.keys()
        }

        /// How far the mouse moved since the last sample.
        ///
        /// The device reports how far it moved rather than where the cursor
        /// ended up, so turning goes on however far the mouse travels in one
        /// direction. Nothing is reported while the run does not hold the
        /// cursor.
        pub fn mouse(&mut self) -> (i16, i16) {
            self.app.run.mouse()
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

    /// A sample takes everything the mouse reported since the last one,
    /// whatever the cursor could have done on the screen in that time.
    #[cfg(feature = "window")]
    #[test]
    fn a_sample_takes_every_report_since_the_last_one() {
        let mut motion = Motion::default();
        for _ in 0..1000 {
            motion.add((7.0, -3.0));
        }
        assert_eq!(motion.drain(), (7000, -3000));
        assert_eq!(motion.drain(), (0, 0));
    }

    /// Movement smaller than one unit is kept rather than rounded away, so
    /// a slow steady drag turns instead of doing nothing.
    #[cfg(feature = "window")]
    #[test]
    fn motion_below_one_unit_carries_to_a_later_sample() {
        let mut motion = Motion::default();
        motion.add((0.4, -0.4));
        assert_eq!(motion.drain(), (0, 0));
        motion.add((0.4, -0.4));
        assert_eq!(motion.drain(), (0, 0));
        motion.add((0.4, -0.4));
        assert_eq!(motion.drain(), (1, -1));
    }

    /// An input row carries an `i16`. More than that in one sample is taken
    /// over several rather than wrapping round into the other direction.
    #[cfg(feature = "window")]
    #[test]
    fn a_sample_takes_no_more_than_an_input_row_carries() {
        let mut motion = Motion::default();
        motion.add((100_000.0, -100_000.0));
        assert_eq!(motion.drain(), (i16::MAX, i16::MIN));
        let (dx, dy) = motion.drain();
        assert!(dx > 0 && dy < 0, "{dx},{dy}");
    }

    /// A run that holds the cursor turns by everything the mouse reported,
    /// however far in one direction it went.
    #[cfg(feature = "window")]
    #[test]
    fn a_held_cursor_turns_by_every_report_however_far_the_mouse_went() {
        let mut run = Run::default();
        run.button(true, winit::event::MouseButton::Left);
        assert!(run.holds_cursor());
        for _ in 0..5000 {
            run.moved((9.0, 0.0));
        }
        assert_eq!(run.mouse(), (i16::MAX, 0));
        let (dx, _) = run.mouse();
        assert!(dx > 0, "the rest of the movement was dropped: {dx}");
    }

    /// Nothing is read while the cursor is free, and nothing carries across
    /// a release, so taking the cursor again does not turn the player by
    /// everything the pointer did on the desktop.
    #[cfg(feature = "window")]
    #[test]
    fn a_free_cursor_reports_no_movement_and_a_release_forgets_what_it_had() {
        let mut run = Run::default();
        run.moved((500.0, -500.0));
        assert_eq!(run.mouse(), (0, 0));

        run.button(true, winit::event::MouseButton::Left);
        run.moved((500.0, -500.0));
        run.key_down(winit::keyboard::KeyCode::Escape);
        assert!(!run.holds_cursor());
        assert_eq!(run.mouse(), (0, 0));
    }

    /// The click that takes the cursor is not also a shot, and the buttons
    /// count once the run has the cursor.
    #[cfg(feature = "window")]
    #[test]
    fn the_click_that_takes_the_cursor_does_not_fire() {
        use clickdoom_spec::native_state::key;
        use winit::event::MouseButton;
        let mut run = Run::default();
        run.button(true, MouseButton::Left);
        assert!(run.holds_cursor());
        assert_eq!(run.keys() & key::FIRE, 0, "the click fired");
        run.button(false, MouseButton::Left);

        run.button(true, MouseButton::Left);
        assert_eq!(run.keys() & key::FIRE, key::FIRE);
        run.button(true, MouseButton::Right);
        assert_eq!(run.keys() & key::USE, key::USE);
        run.button(false, MouseButton::Left);
        run.button(false, MouseButton::Right);
        assert_eq!(run.keys() & (key::FIRE | key::USE), 0);
    }

    /// Two keys onto one bit: the bit stays set while either is down.
    #[cfg(feature = "window")]
    #[test]
    fn a_bit_two_keys_share_survives_one_of_them_coming_up() {
        use clickdoom_spec::native_state::key;
        use winit::keyboard::KeyCode;
        let mut run = Run::default();
        run.key_down(KeyCode::KeyW);
        run.key_down(KeyCode::ArrowUp);
        run.key_up(KeyCode::KeyW);
        assert_eq!(run.keys() & key::UP, key::UP);
        run.key_up(KeyCode::ArrowUp);
        assert_eq!(run.keys() & key::UP, 0);
    }

    /// The pause key is a press. Holding it down pauses once, not once per
    /// tic, and a press and release inside one tic is not lost.
    #[cfg(feature = "window")]
    #[test]
    fn the_pause_key_sets_its_bit_once_per_press() {
        use clickdoom_spec::native_state::key;
        use winit::keyboard::KeyCode;
        let mut run = Run::default();
        run.key_down(KeyCode::KeyP);
        assert_eq!(run.keys() & key::PAUSE, key::PAUSE);
        assert_eq!(run.keys() & key::PAUSE, 0, "held down, it paused twice");

        run.key_down(KeyCode::KeyP);
        run.key_up(KeyCode::KeyP);
        assert_eq!(
            run.keys() & key::PAUSE,
            key::PAUSE,
            "a quick press was lost"
        );
    }

    /// The lowest weapon key down wins, and no key means no weapon.
    #[cfg(feature = "window")]
    #[test]
    fn one_weapon_key_at_a_time_reaches_the_command() {
        use clickdoom_spec::native_state::key;
        use winit::keyboard::KeyCode;
        let mut run = Run::default();
        assert_eq!(run.keys() & key::WEAPON_MASK, 0);
        run.key_down(KeyCode::Digit3);
        assert_eq!(run.keys() & key::WEAPON_MASK, 3 << key::WEAPON_SHIFT);
        run.key_down(KeyCode::Digit5);
        assert_eq!(run.keys() & key::WEAPON_MASK, 3 << key::WEAPON_SHIFT);
        run.key_up(KeyCode::Digit3);
        assert_eq!(run.keys() & key::WEAPON_MASK, 5 << key::WEAPON_SHIFT);
    }

    /// A key held when the window loses focus does not stay down, because
    /// no release for it ever arrives.
    #[cfg(feature = "window")]
    #[test]
    fn losing_focus_lets_go_of_every_key() {
        use winit::keyboard::KeyCode;
        let mut run = Run::default();
        run.key_down(KeyCode::KeyW);
        run.button(true, winit::event::MouseButton::Left);
        run.button(true, winit::event::MouseButton::Left);
        assert_ne!(run.keys(), 0);
        run.unfocused();
        assert_eq!(run.keys(), 0);
        assert!(!run.holds_cursor());
        assert!(run.open(), "losing focus ended the run");
    }

    /// Escape frees the cursor and the run goes on. Escape with the cursor
    /// already free ends it.
    #[cfg(feature = "window")]
    #[test]
    fn escape_frees_the_cursor_before_it_ends_the_run() {
        use winit::keyboard::KeyCode;
        let mut run = Run::default();
        run.button(true, winit::event::MouseButton::Left);
        assert!(run.holds_cursor() && run.open());
        run.key_down(KeyCode::Escape);
        assert!(!run.holds_cursor() && run.open());
        run.key_down(KeyCode::Escape);
        assert!(!run.open());
    }

    /// The cursor is free until a click, Escape gives it back, and Escape
    /// with it already free ends the run.
    #[cfg(feature = "window")]
    #[test]
    fn escape_frees_the_cursor_and_escape_again_ends_the_run() {
        let start = Grab::default();
        assert_eq!(start, Grab::Released);
        assert!(!start.holds_cursor() && start.open());

        let locked = start.clicked();
        assert_eq!(locked, Grab::Locked);
        assert!(locked.holds_cursor() && locked.open());

        let freed = locked.escaped();
        assert_eq!(freed, Grab::Released);
        assert!(!freed.holds_cursor() && freed.open());

        let closing = freed.escaped();
        assert_eq!(closing, Grab::Closing);
        assert!(!closing.holds_cursor() && !closing.open());
    }

    /// Losing focus frees the cursor. Getting focus back does not take it
    /// again, so a window the pointer is over is safe to click through.
    #[cfg(feature = "window")]
    #[test]
    fn losing_focus_frees_the_cursor_and_only_a_click_takes_it_back() {
        assert_eq!(Grab::Locked.unfocused(), Grab::Released);
        assert_eq!(Grab::Released.unfocused(), Grab::Released);
        assert_eq!(Grab::Released.clicked(), Grab::Locked);
        assert_eq!(Grab::Locked.clicked(), Grab::Locked);
    }

    /// A run that is closing stays closed whatever else arrives.
    #[cfg(feature = "window")]
    #[test]
    fn nothing_reopens_a_run_that_is_closing() {
        assert_eq!(Grab::Closing.clicked(), Grab::Closing);
        assert_eq!(Grab::Closing.unfocused(), Grab::Closing);
        assert_eq!(Grab::Closing.escaped(), Grab::Closing);
    }

    /// A screen exactly the frame's size takes the frame's words in the
    /// frame's order.
    #[cfg(feature = "window")]
    #[test]
    fn a_screen_the_size_of_the_frame_takes_it_word_for_word() {
        let frame: Vec<u32> = (0..WIDTH * HEIGHT).map(|word| word as u32).collect();
        let mut screen = vec![0xdead_beef; WIDTH * HEIGHT];
        blit(&frame, &mut screen, WIDTH, HEIGHT);
        assert_eq!(screen, frame);
    }

    /// A bigger screen repeats each word over the square of screen pixels it
    /// is drawn at, and puts nothing else anywhere.
    #[cfg(feature = "window")]
    #[test]
    fn a_bigger_screen_repeats_each_word_and_invents_none() {
        let frame: Vec<u32> = (0..WIDTH * HEIGHT)
            .map(|word| word as u32 | 0x0010_0000)
            .collect();
        let (width, height) = (WIDTH * 3, HEIGHT * 3);
        let mut screen = vec![0xdead_beef; width * height];
        blit(&frame, &mut screen, width, height);
        for y in 0..height {
            for x in 0..width {
                assert_eq!(
                    screen[y * width + x],
                    frame[(y / 3) * WIDTH + x / 3],
                    "at {x},{y}"
                );
            }
        }
    }

    /// A screen that is not a whole multiple of the frame draws the frame
    /// centred at the scale that fits and leaves the rest black.
    #[cfg(feature = "window")]
    #[test]
    fn a_screen_that_does_not_divide_evenly_gets_black_around_the_frame() {
        let frame = vec![0x00ff_ffff; WIDTH * HEIGHT];
        let (width, height) = (WIDTH * 2 + 10, HEIGHT * 2 + 6);
        let mut screen = vec![0xdead_beef; width * height];
        blit(&frame, &mut screen, width, height);
        assert_eq!(screen[0], 0, "the top left corner is not black");
        assert_eq!(screen[width * height - 1], 0, "the bottom right one is not");
        let lit = screen.iter().filter(|word| **word == 0x00ff_ffff).count();
        assert_eq!(lit, WIDTH * 2 * HEIGHT * 2);
        assert_eq!(
            screen[3 * width + 5],
            0x00ff_ffff,
            "the frame is off centre"
        );
    }

    /// A screen too small for one whole frame pixel is left black rather
    /// than drawn into past its end.
    #[cfg(feature = "window")]
    #[test]
    fn a_screen_smaller_than_the_frame_is_left_black() {
        let frame = vec![0x00ff_ffff; WIDTH * HEIGHT];
        let mut screen = vec![0xdead_beef; 16 * 16];
        blit(&frame, &mut screen, 16, 16);
        assert!(screen.iter().all(|word| *word == 0));
    }
}
