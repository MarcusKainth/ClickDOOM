//! The fixed tic clock a paced run follows.
//!
//! Reporting and pacing only. Nothing here reaches a statement or a
//! computed value: what the clock decides is when the driver sends the next
//! row, never what is in it.

use std::time::Duration; // purity-ok: the pacing arithmetic, applied to a clock the caller reads

use clickdoom_spec::native_state::TICRATE;

/// One tic at the engine's own rate.
pub const TIC: Duration = Duration::from_nanos(1_000_000_000 / TICRATE as u64);

/// Where the next tic is due, and how many have been late.
///
/// A tic that overruns re-bases the deadline rather than borrowing from the
/// tics after it, so a slow one costs its own lateness and nothing more:
/// there is never a catch-up burst, and no tic is ever skipped.
pub struct Pace {
    period: Duration,
    /// When the next tic is due, on the clock the caller reads.
    deadline: Duration,
    late: u64,
}

impl Pace {
    /// Starts the clock with the first tic due one period after `now`.
    pub fn start(period: Duration, now: Duration) -> Pace {
        Pace {
            period,
            deadline: now + period,
            late: 0,
        }
    }

    /// How long to wait for the next tic, moving the deadline on.
    ///
    /// Zero when the deadline has already passed, which counts the tic
    /// late.
    pub fn wait_for_next(&mut self, now: Duration) -> Duration {
        let rest = self.deadline.saturating_sub(now);
        if now > self.deadline {
            self.late += 1;
        }
        self.deadline = (self.deadline + self.period).max(now);
        rest
    }

    /// Tics that were not ready by their deadline.
    pub fn late(&self) -> u64 {
        self.late
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn a_tic_is_the_engines_own_rate() {
        assert_eq!(TICRATE, 35);
        assert_eq!(TIC, Duration::from_nanos(28_571_428));
    }

    #[test]
    fn a_tic_that_finishes_early_waits_out_the_rest_of_its_period() {
        let mut pace = Pace::start(ms(100), Duration::ZERO);
        assert_eq!(pace.wait_for_next(ms(40)), ms(60));
        assert_eq!(pace.wait_for_next(ms(140)), ms(60));
        assert_eq!(pace.late(), 0);
    }

    /// The deadline moves by one period whatever the tic cost, so the tics
    /// after a slow one are not squeezed into what is left of its period.
    #[test]
    fn a_tic_that_overruns_is_counted_and_the_next_one_keeps_its_own_period() {
        let mut pace = Pace::start(ms(100), Duration::ZERO);
        assert_eq!(pace.wait_for_next(ms(130)), Duration::ZERO);
        assert_eq!(pace.late(), 1);
        // The next deadline is 200 ms, not 130 ms: 70 ms from here.
        assert_eq!(pace.wait_for_next(ms(130)), ms(70));
        assert_eq!(pace.late(), 1);
    }

    /// A tic that overruns by more than a whole period re-bases the
    /// deadline to now, so the tics after it are not all late against a
    /// deadline that is already in the past.
    #[test]
    fn an_overrun_longer_than_a_period_re_bases_the_clock() {
        let mut pace = Pace::start(ms(100), Duration::ZERO);
        assert_eq!(pace.wait_for_next(ms(450)), Duration::ZERO);
        assert_eq!(pace.late(), 1);

        // The deadline is 450 ms, not 200 ms, so the next tic is due now
        // and the one after it a full period later.
        assert_eq!(pace.wait_for_next(ms(450)), Duration::ZERO);
        assert_eq!(pace.late(), 1, "landing on the deadline is not late");
        assert_eq!(pace.wait_for_next(ms(450)), ms(100));
        assert_eq!(pace.late(), 1);
    }

    /// A tic that lands exactly on its deadline is on time.
    #[test]
    fn landing_on_the_deadline_is_not_late() {
        let mut pace = Pace::start(ms(100), Duration::ZERO);
        assert_eq!(pace.wait_for_next(ms(100)), Duration::ZERO);
        assert_eq!(pace.late(), 0);
    }
}
