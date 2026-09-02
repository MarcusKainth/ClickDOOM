//! The progress line a long-running mode prints while it works.
//!
//! Reporting only. The clock is read to rate-limit the line and to divide by,
//! and nothing here reaches a statement or a committed result, so every read
//! carries a `purity-ok:` annotation saying which.
//!
//! Two lines, one per mode. [`StatsLine`] counts instructions for a run of
//! the CPU in SQL; [`NativeStatsLine`] counts tics and frames for a paced
//! run of DOOM's own simulation and renderer. Both report what the window
//! they cover actually did, never a target.

use std::time::{Duration, Instant}; // purity-ok: rate reporting on stderr, no emulator result depends on it

/// Instructions per second over a window. A window with no time in it has no
/// rate and reports 0.0, rather than the infinity a bare division gives.
pub fn instr_per_sec(retired: u64, seconds: f64) -> f64 {
    if seconds > 0.0 {
        retired as f64 / seconds
    } else {
        0.0
    }
}

/// A monotonic time source. [`StatsLine`] is generic over it so a test can
/// drive the gating without waiting for real time.
pub trait Clock {
    /// Time since an origin the clock picks. Never goes backwards.
    fn elapsed(&self) -> Duration;
}

/// The process's own monotonic clock.
pub struct Monotonic {
    origin: Instant,
}

impl Monotonic {
    pub fn new() -> Self {
        Monotonic {
            origin: Instant::now(), // purity-ok: the reporting origin, read by no query
        }
    }
}

impl Default for Monotonic {
    fn default() -> Self {
        Monotonic::new()
    }
}

impl Clock for Monotonic {
    fn elapsed(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// What a run has done since this process started. A resumed run counts from
/// where it resumed, so the rates describe this process rather than the whole
/// history behind it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub instructions: u64,
    pub batches: u64,
    pub frames: u64,
}

/// A `key=value` progress line, emitted at most once per interval.
///
/// A progress line carries `instr_per_sec`, the rate over the window since
/// the previous line, and `instr_per_sec_mean`, the rate since the run
/// started, so a run that slows down shows it in the first field before the
/// second moves. The closing line is marked `final` and carries only the
/// mean, because a run ends where it ends and its last window is not a
/// window of anything.
pub struct StatsLine<C: Clock> {
    clock: C,
    interval: Duration,
    started: Duration,
    window_started: Duration,
    window_instructions: u64,
}

impl<C: Clock> StatsLine<C> {
    /// Starts reporting at the clock's current reading. Nothing comes out
    /// until `interval` has passed.
    pub fn start(clock: C, interval: Duration) -> Self {
        let now = clock.elapsed();
        StatsLine {
            clock,
            interval,
            started: now,
            window_started: now,
            window_instructions: 0,
        }
    }

    /// The line to print, or `None` while less than `interval` has passed
    /// since the last one. The caller prints it.
    pub fn tick(&mut self, counters: Counters) -> Option<String> {
        let now = self.clock.elapsed();
        if now.saturating_sub(self.window_started) < self.interval {
            return None;
        }
        Some(self.close_window(counters, now))
    }

    /// The run's totals, whatever the interval says.
    pub fn finish(&self, counters: Counters) -> String {
        let seconds = self
            .clock
            .elapsed()
            .saturating_sub(self.started)
            .as_secs_f64();
        format!(
            "# stats final elapsed={seconds:.1}s instr={} instr_per_sec_mean={:.1} batches={} frames={}",
            counters.instructions,
            instr_per_sec(counters.instructions, seconds),
            counters.batches,
            counters.frames,
        )
    }

    fn close_window(&mut self, counters: Counters, now: Duration) -> String {
        let window_seconds = now.saturating_sub(self.window_started).as_secs_f64();
        let window_instructions = counters
            .instructions
            .saturating_sub(self.window_instructions);
        let total_seconds = now.saturating_sub(self.started).as_secs_f64();
        self.window_started = now;
        self.window_instructions = counters.instructions;
        format!(
            "# stats elapsed={total_seconds:.1}s instr={} instr_per_sec={:.1} instr_per_sec_mean={:.1} batches={} frames={}",
            counters.instructions,
            instr_per_sec(window_instructions, window_seconds),
            instr_per_sec(counters.instructions, total_seconds),
            counters.batches,
            counters.frames,
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone)]
    pub(super) struct FakeClock(Rc<Cell<Duration>>);

    impl FakeClock {
        pub(super) fn new() -> Self {
            FakeClock(Rc::new(Cell::new(Duration::ZERO)))
        }

        pub(super) fn advance(&self, by: Duration) {
            self.0.set(self.0.get() + by);
        }
    }

    impl Clock for FakeClock {
        fn elapsed(&self) -> Duration {
            self.0.get()
        }
    }

    fn second() -> Duration {
        Duration::from_secs(1)
    }

    /// The value of one `key=value` pair, so a test names the field it means
    /// rather than an offset into the line.
    fn field<'a>(line: &'a str, key: &str) -> &'a str {
        let prefix = format!("{key}=");
        line.split(' ')
            .find_map(|pair| pair.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("no {key}= in {line:?}"))
    }

    fn counters(instructions: u64, batches: u64, frames: u64) -> Counters {
        Counters {
            instructions,
            batches,
            frames,
        }
    }

    #[test]
    fn a_rate_is_instructions_over_seconds() {
        assert_eq!(instr_per_sec(1000, 2.0), 500.0);
        assert_eq!(instr_per_sec(0, 2.0), 0.0);
    }

    #[test]
    fn a_window_with_no_time_in_it_has_no_rate() {
        assert_eq!(instr_per_sec(1000, 0.0), 0.0);
        assert_eq!(instr_per_sec(1000, -1.0), 0.0);
    }

    #[test]
    fn nothing_comes_out_before_the_interval_has_passed() {
        let clock = FakeClock::new();
        let mut stats = StatsLine::start(clock.clone(), second());
        assert!(stats.tick(counters(10, 1, 0)).is_none());
        clock.advance(Duration::from_millis(999));
        assert!(stats.tick(counters(20, 2, 0)).is_none());
        clock.advance(Duration::from_millis(1));
        assert!(stats.tick(counters(30, 3, 0)).is_some());
    }

    #[test]
    fn the_gate_follows_the_clock_and_not_the_call_count() {
        let clock = FakeClock::new();
        let mut stats = StatsLine::start(clock.clone(), second());
        for _ in 0..1000 {
            assert!(stats.tick(counters(1, 1, 0)).is_none());
        }
        clock.advance(second());
        assert!(stats.tick(counters(1, 1, 0)).is_some());
    }

    #[test]
    fn the_window_rate_covers_the_window_and_the_mean_covers_the_run() {
        let clock = FakeClock::new();
        let mut stats = StatsLine::start(clock.clone(), second());

        clock.advance(second());
        let first = stats.tick(counters(1000, 1, 0)).expect("first line");
        assert_eq!(field(&first, "instr_per_sec"), "1000.0");
        assert_eq!(field(&first, "instr_per_sec_mean"), "1000.0");

        clock.advance(second());
        let second_line = stats.tick(counters(3000, 2, 0)).expect("second line");
        assert_eq!(field(&second_line, "instr_per_sec"), "2000.0");
        assert_eq!(field(&second_line, "instr_per_sec_mean"), "1500.0");
        assert_eq!(field(&second_line, "elapsed"), "2.0s");
    }

    #[test]
    fn the_line_carries_the_counts_it_was_given() {
        let clock = FakeClock::new();
        let mut stats = StatsLine::start(clock.clone(), second());
        clock.advance(second());
        let line = stats.tick(counters(500, 7, 3)).expect("a line");
        assert!(line.starts_with("# stats "), "{line:?}");
        assert_eq!(field(&line, "instr"), "500");
        assert_eq!(field(&line, "batches"), "7");
        assert_eq!(field(&line, "frames"), "3");
    }

    #[test]
    fn finish_reports_a_run_shorter_than_the_interval() {
        let clock = FakeClock::new();
        let mut stats = StatsLine::start(clock.clone(), second());
        clock.advance(Duration::from_millis(500));
        assert!(stats.tick(counters(100, 1, 0)).is_none());
        let line = stats.finish(counters(100, 1, 0));
        assert!(line.starts_with("# stats final "), "{line:?}");
        assert_eq!(field(&line, "instr"), "100");
        assert_eq!(field(&line, "instr_per_sec_mean"), "200.0");
    }

    /// The closing line covers the whole run, so it does not depend on where
    /// the last window happened to end.
    #[test]
    fn finish_covers_the_run_and_not_the_last_window() {
        let clock = FakeClock::new();
        let mut stats = StatsLine::start(clock.clone(), second());
        clock.advance(second());
        assert!(stats.tick(counters(1000, 1, 0)).is_some());
        let line = stats.finish(counters(1000, 1, 0));
        assert_eq!(field(&line, "instr_per_sec_mean"), "1000.0");
        assert!(!line.contains("instr_per_sec="), "{line:?}");
    }

    #[test]
    fn a_zero_length_run_reports_a_zero_rate_rather_than_an_infinity() {
        let clock = FakeClock::new();
        let stats = StatsLine::start(clock, second());
        let line = stats.finish(counters(0, 0, 0));
        assert_eq!(field(&line, "instr_per_sec_mean"), "0.0");
    }
}

/// What a paced native run has done since this process started.
///
/// The durations are totals: the line divides them by the frames or tics of
/// the window it covers, so what comes out is the mean cost of one.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeCounters {
    pub tics: u64,
    pub frames: u64,
    /// From a tic's row being sent to its state row being readable, over
    /// every tic. Absent for a run whose states came from somewhere else.
    pub sim: Option<Duration>,
    /// From a frame's row being sent to the frame being readable.
    pub render: Duration,
    /// The read-backs that carried the frames, inside that.
    pub poll: Duration,
    /// Putting the frames on the screen.
    pub blit: Duration,
    /// Writing the frames out as files, which is a query per frame and not
    /// something a run at 35 Hz does.
    pub write: Duration,
    /// Tics the simulation is ahead of the frame being drawn. Absent for a
    /// run with no simulation.
    pub lookahead: Option<u32>,
    /// Tics that were not ready by their deadline.
    pub late: u64,
}

/// A `key=value` progress line for a paced native run, emitted at most once
/// per interval.
///
/// `tics_per_sec` and `fps` are what the window since the previous line
/// achieved, not the rate it was aiming for, so a run that cannot hold 35 Hz
/// says so. The closing line is marked `final` and covers the whole run.
pub struct NativeStatsLine<C: Clock> {
    clock: C,
    interval: Duration,
    started: Duration,
    origin: NativeCounters,
    window_started: Duration,
    window: NativeCounters,
}

impl<C: Clock> NativeStatsLine<C> {
    /// Starts reporting at the clock's current reading, with the run
    /// standing at `at`. Nothing comes out until `interval` has passed.
    ///
    /// `at` is what a run that did something before it started pacing
    /// itself passes: the rates then cover the paced part, and the work
    /// before it neither counts towards them nor against them.
    pub fn start(clock: C, interval: Duration, at: NativeCounters) -> Self {
        let now = clock.elapsed();
        NativeStatsLine {
            clock,
            interval,
            started: now,
            origin: at,
            window_started: now,
            window: at,
        }
    }

    /// The line to print, or `None` while less than `interval` has passed
    /// since the last one. The caller prints it.
    pub fn tick(&mut self, counters: NativeCounters) -> Option<String> {
        let now = self.clock.elapsed();
        if now.saturating_sub(self.window_started) < self.interval {
            return None;
        }
        let line = line(
            "",
            now.saturating_sub(self.window_started),
            &self.window,
            &counters,
        );
        self.window_started = now;
        self.window = counters;
        Some(line)
    }

    /// The run's totals, whatever the interval says.
    pub fn finish(&self, counters: NativeCounters) -> String {
        line(
            "final ",
            self.clock.elapsed().saturating_sub(self.started),
            &self.origin,
            &counters,
        )
    }
}

/// One line over the span from `before` to `now`, `seconds` long.
fn line(kind: &str, span: Duration, before: &NativeCounters, now: &NativeCounters) -> String {
    let seconds = span.as_secs_f64();
    let tics = now.tics.saturating_sub(before.tics);
    let frames = now.frames.saturating_sub(before.frames);
    let mut out = format!(
        "# native {kind}elapsed={seconds:.1}s tics/s={:.1} fps={:.1}",
        per_sec(tics, seconds),
        per_sec(frames, seconds),
    );
    if let Some(sim) = now.sim {
        let was = before.sim.unwrap_or_default();
        out.push_str(&format!(
            " sim={:.1}ms",
            mean_ms(sim.saturating_sub(was), tics)
        ));
    }
    out.push_str(&format!(
        " render={:.1}ms poll={:.1}ms blit={:.1}ms write={:.1}ms",
        mean_ms(now.render.saturating_sub(before.render), frames),
        mean_ms(now.poll.saturating_sub(before.poll), frames),
        mean_ms(now.blit.saturating_sub(before.blit), frames),
        mean_ms(now.write.saturating_sub(before.write), frames),
    ));
    if let Some(lookahead) = now.lookahead {
        out.push_str(&format!(" lookahead={lookahead}"));
    }
    out.push_str(&format!(
        " late={} tics={} frames={}",
        now.late, now.tics, now.frames
    ));
    out
}

/// Events per second. A window with no time in it has no rate and reports
/// 0.0, rather than the infinity a bare division gives.
fn per_sec(count: u64, seconds: f64) -> f64 {
    if seconds > 0.0 {
        count as f64 / seconds
    } else {
        0.0
    }
}

/// The mean of `total` over `count`, in milliseconds. Zero events cost
/// nothing on average rather than an infinity.
fn mean_ms(total: Duration, count: u64) -> f64 {
    if count > 0 {
        total.as_secs_f64() * 1e3 / count as f64
    } else {
        0.0
    }
}

#[cfg(test)]
mod native_tests {
    use super::tests::FakeClock;
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn field<'a>(line: &'a str, key: &str) -> &'a str {
        let prefix = format!("{key}=");
        line.split(' ')
            .find_map(|pair| pair.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("no {key}= in {line:?}"))
    }

    fn counters(tics: u64, frames: u64) -> NativeCounters {
        NativeCounters {
            tics,
            frames,
            render: ms(20) * frames as u32,
            poll: ms(2) * frames as u32,
            blit: ms(1) * frames as u32,
            write: ms(4) * frames as u32,
            ..NativeCounters::default()
        }
    }

    #[test]
    fn the_line_reports_the_rate_the_window_achieved() {
        let clock = FakeClock::new();
        let mut stats = NativeStatsLine::start(
            clock.clone(),
            Duration::from_secs(1),
            NativeCounters::default(),
        );
        assert!(stats.tick(counters(10, 10)).is_none());

        clock.advance(Duration::from_secs(1));
        let line = stats.tick(counters(35, 35)).expect("a line");
        assert!(line.starts_with("# native elapsed="), "{line:?}");
        assert_eq!(field(&line, "tics/s"), "35.0");
        assert_eq!(field(&line, "fps"), "35.0");
        assert_eq!(field(&line, "render"), "20.0ms");
        assert_eq!(field(&line, "poll"), "2.0ms");
        assert_eq!(field(&line, "blit"), "1.0ms");
        assert_eq!(field(&line, "write"), "4.0ms");
        assert_eq!(field(&line, "late"), "0");
        assert_eq!(field(&line, "frames"), "35");
    }

    /// A run that cannot hold the rate says the rate it held.
    #[test]
    fn a_window_that_fell_behind_reports_what_it_managed() {
        let clock = FakeClock::new();
        let mut stats = NativeStatsLine::start(
            clock.clone(),
            Duration::from_secs(1),
            NativeCounters::default(),
        );
        clock.advance(Duration::from_secs(2));
        let mut slow = counters(40, 40);
        slow.late = 12;
        let line = stats.tick(slow).expect("a line");
        assert_eq!(field(&line, "fps"), "20.0");
        assert_eq!(field(&line, "late"), "12");
    }

    /// A run with no simulation has no simulation cost to report, and says
    /// nothing rather than 0.0.
    #[test]
    fn a_run_without_a_simulation_names_neither_it_nor_a_lookahead() {
        let clock = FakeClock::new();
        let mut stats = NativeStatsLine::start(
            clock.clone(),
            Duration::from_secs(1),
            NativeCounters::default(),
        );
        clock.advance(Duration::from_secs(1));
        let line = stats.tick(counters(35, 35)).expect("a line");
        assert!(!line.contains("sim="), "{line:?}");
        assert!(!line.contains("lookahead="), "{line:?}");
    }

    #[test]
    fn a_run_with_a_simulation_reports_what_a_tic_cost_and_how_far_ahead_it_is() {
        let clock = FakeClock::new();
        let mut stats = NativeStatsLine::start(
            clock.clone(),
            Duration::from_secs(1),
            NativeCounters::default(),
        );
        let mut counters = counters(0, 0);
        counters.sim = Some(Duration::ZERO);
        counters.lookahead = Some(0);
        assert!(stats.tick(counters).is_none());

        clock.advance(Duration::from_secs(1));
        counters = self::counters(35, 35);
        counters.sim = Some(ms(5) * 35);
        counters.lookahead = Some(35);
        let line = stats.tick(counters).expect("a line");
        assert_eq!(field(&line, "sim"), "5.0ms");
        assert_eq!(field(&line, "lookahead"), "35");
    }

    #[test]
    fn the_closing_line_covers_the_run_and_is_marked_final() {
        let clock = FakeClock::new();
        let mut stats = NativeStatsLine::start(
            clock.clone(),
            Duration::from_secs(1),
            NativeCounters::default(),
        );
        clock.advance(Duration::from_secs(1));
        assert!(stats.tick(counters(35, 35)).is_some());
        clock.advance(Duration::from_secs(1));
        let line = stats.finish(counters(70, 70));
        assert!(line.starts_with("# native final "), "{line:?}");
        assert_eq!(field(&line, "fps"), "35.0");
        assert_eq!(field(&line, "frames"), "70");
    }

    /// A run that warmed up before it started pacing reports the paced
    /// part, so the warm-up neither counts towards the rate nor against it.
    #[test]
    fn the_rates_cover_the_run_from_where_it_started_counting() {
        let clock = FakeClock::new();
        let stats = NativeStatsLine::start(clock.clone(), Duration::from_secs(1), counters(1, 1));
        clock.advance(Duration::from_secs(1));
        let line = stats.finish(counters(36, 36));
        assert_eq!(field(&line, "fps"), "35.0");
        assert_eq!(field(&line, "frames"), "36", "the count is the whole run");
    }

    #[test]
    fn a_window_with_no_time_or_no_frames_in_it_reports_zero_rather_than_an_infinity() {
        assert_eq!(per_sec(35, 0.0), 0.0);
        assert_eq!(mean_ms(ms(20), 0), 0.0);
        let clock = FakeClock::new();
        let stats =
            NativeStatsLine::start(clock, Duration::from_secs(1), NativeCounters::default());
        let line = stats.finish(NativeCounters::default());
        assert_eq!(field(&line, "fps"), "0.0");
        assert_eq!(field(&line, "render"), "0.0ms");
    }
}
