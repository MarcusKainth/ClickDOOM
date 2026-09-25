//! Many seeded arms driven through one [`Session`], so a suite pays the tic
//! statement's analysis once for all of them.
//!
//! An arm is a state row seeded at `at` from a walked row, then its own
//! inputs fed on the tics `at + 1` to `at + length`. A tic reads
//! `native_state` only at the tic before and `native_stage` only at its
//! own tic, so arms whose tic ranges share no tic read none of each
//! other's rows. The demo lump and the melt are read by absolute tic, so
//! an arm is refused both.

use std::collections::BTreeSet;
use std::fmt::{Debug, Display};
use std::ops::RangeInclusive;

use clickdoom_native::sql::sim::tick::Input;
use clickdoom_native::sql::{self, sim::tick};

use super::db::Fixture;
use super::resident::{self, Session};
use super::seed;

pub struct Arm {
    pub name: &'static str,
    /// The walked tic the seed copies.
    pub from: u32,
    pub overrides: Vec<(&'static str, String)>,
    /// The tic the seeded row lands on.
    pub at: u32,
    /// The inputs for the tics `at + 1` onwards, one per tic, driven by
    /// keys.
    pub inputs: Vec<Input>,
}

impl Arm {
    /// The tics the arm's rows sit on: its seed and every tic fed from it.
    pub fn tics(&self) -> RangeInclusive<u32> {
        self.at..=self.at + self.inputs.len() as u32
    }

    /// Every row `table` holds on the arm's own tics, in tic order.
    /// `columns` is a select list over the table's columns.
    pub async fn rows<T>(&self, fixture: &Fixture, table: &str, columns: &str) -> Vec<T>
    where
        T: clickhouse::RowOwned + clickhouse::RowRead,
    {
        let tics = self.tics();
        fixture
            .rows(&format!(
                "SELECT {columns} FROM {}.{table} WHERE tic BETWEEN {} AND {} ORDER BY tic",
                fixture.database,
                tics.start(),
                tics.end()
            ))
            .await
    }
}

/// A walk from tic 1 and the arms seeded from it, checked to share no tic.
pub struct Arms {
    walk: Vec<Input>,
    arms: Vec<Arm>,
}

impl Arms {
    /// Panics unless the walk feeds the tics 1 to its length in order, and
    /// every arm is keys-driven, fed the tics after its own seed in order,
    /// seeded from a walked tic, and on tics no other arm, the walk, tic 0
    /// or the melt's [`tick::MELT_TIC`] reaches.
    pub fn new(walk: Vec<Input>, arms: Vec<Arm>) -> Arms {
        for (input, tic) in walk.iter().zip(1..) {
            assert_eq!(input.tic, tic, "the walk feeds the tics from 1 in order");
        }
        let walked = 0..=walk.len() as u32;
        let mut names = BTreeSet::new();
        let reserved = 0..=(*walked.end()).max(tick::MELT_TIC);
        let mut taken: Vec<(RangeInclusive<u32>, &str)> = vec![(reserved, "the walk")];
        for arm in &arms {
            assert!(names.insert(arm.name), "two arms are named {}", arm.name);
            assert!(!arm.inputs.is_empty(), "arm {} feeds no tic", arm.name);
            for (input, tic) in arm.inputs.iter().zip(arm.at + 1..) {
                assert!(
                    input.source == tick::source::KEYS,
                    "arm {} is driven by the demo at tic {}, which the demo lump reads by \
                     absolute tic; it needs a session of its own",
                    arm.name,
                    input.tic
                );
                assert_eq!(input.tic, tic, "arm {} feeds its tics in order", arm.name);
            }
            assert!(
                walked.contains(&arm.from),
                "arm {} seeds from tic {}, which the walk does not reach",
                arm.name,
                arm.from
            );
            let tics = arm.tics();
            for (other, owner) in &taken {
                assert!(
                    tics.end() < other.start() || other.end() < tics.start(),
                    "arm {} on tics {tics:?} meets {owner} on {other:?}",
                    arm.name
                );
            }
            taken.push((tics, arm.name));
        }
        Arms { walk, arms }
    }

    pub fn all(&self) -> &[Arm] {
        &self.arms
    }

    pub fn arm(&self, name: &str) -> &Arm {
        self.arms
            .iter()
            .find(|arm| arm.name == name)
            .unwrap_or_else(|| panic!("no arm is named {name}"))
    }

    /// Feeds the walk, then seeds each arm and feeds it before seeding the
    /// next.
    pub async fn drive(&self, fixture: &Fixture, session: &mut Session<'_>) {
        session.feed(&self.walk).await;
        for arm in &self.arms {
            let seeded: Vec<sql::Statement> =
                seed::row(&fixture.database, arm.at, arm.from, &arm.overrides)
                    .into_iter()
                    .map(sql::Statement::sql)
                    .collect();
            if let Err(error) = fixture.execute(&seeded).await {
                panic!("seeding arm {}: {error}", arm.name);
            }
            assert!(
                resident::present(fixture, "native_state", arm.at).await,
                "arm {}'s seed left no row at tic {}",
                arm.name,
                arm.at
            );
            session.feed(&arm.inputs).await;
        }
    }

    /// A collector expecting one [`ArmChecks`] per arm.
    pub fn checks(&self) -> Checks {
        Checks {
            declared: self.arms.iter().map(|arm| arm.name).collect(),
            ran: Vec::new(),
            failures: Vec::new(),
            finished: false,
        }
    }
}

/// Every arm's failed assertions, reported together by [`Checks::finish`].
///
/// `finish` also fails for a declared arm that no [`ArmChecks`] was opened
/// for, or that recorded no check. Dropping a collector without calling
/// `finish` panics.
pub struct Checks {
    declared: Vec<&'static str>,
    ran: Vec<(&'static str, usize)>,
    failures: Vec<String>,
    finished: bool,
}

impl Checks {
    /// Opens `arm`'s checks. Panics for an arm not declared, or opened
    /// twice.
    pub fn arm(&mut self, arm: &'static str) -> ArmChecks<'_> {
        assert!(self.declared.contains(&arm), "no arm is named {arm}");
        assert!(
            self.ran.iter().all(|(name, _)| *name != arm),
            "arm {arm}'s checks are opened twice"
        );
        self.ran.push((arm, 0));
        ArmChecks {
            index: self.ran.len() - 1,
            checks: self,
        }
    }

    pub fn finish(mut self) {
        self.finished = true;
        let mut report = std::mem::take(&mut self.failures);
        for arm in &self.declared {
            match self.ran.iter().find(|(name, _)| name == arm) {
                None => report.push(format!("{arm}: never checked")),
                Some((_, 0)) => report.push(format!("{arm}: recorded no check")),
                Some(_) => {}
            }
        }
        let checked = self.ran.iter().filter(|(_, count)| *count > 0).count();
        assert!(
            report.is_empty() && checked == self.declared.len(),
            "{checked} of {} arms checked, {} failures:\n{}",
            self.declared.len(),
            report.len(),
            report.join("\n")
        );
    }
}

impl Drop for Checks {
    fn drop(&mut self) {
        if !self.finished && !std::thread::panicking() {
            panic!("Checks dropped without finish(), so no arm's failures were reported");
        }
    }
}

/// One arm's checks, recorded into its [`Checks`].
pub struct ArmChecks<'a> {
    index: usize,
    checks: &'a mut Checks,
}

impl ArmChecks<'_> {
    pub fn check(&mut self, ok: bool, what: impl Display) {
        let (arm, count) = &mut self.checks.ran[self.index];
        *count += 1;
        if !ok {
            self.checks.failures.push(format!("{arm}: {what}"));
        }
    }

    pub fn eq<T: PartialEq + Debug>(&mut self, got: T, want: T, what: impl Display) {
        let ok = got == want;
        self.check(ok, format_args!("{what}: got {got:?}, want {want:?}"));
    }
}
