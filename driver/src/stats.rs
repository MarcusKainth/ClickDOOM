//! The progress line a long-running mode prints while it works.
//!
//! Reporting only. The clock is read to rate-limit the line and to divide by,
//! and nothing here reaches a statement or a committed result, so every read
//! carries a `purity-ok:` annotation saying which.

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
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[derive(Clone)]
    struct FakeClock(Rc<Cell<Duration>>);

    impl FakeClock {
        fn new() -> Self {
            FakeClock(Rc::new(Cell::new(Duration::ZERO)))
        }

        fn advance(&self, by: Duration) {
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
