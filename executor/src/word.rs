//! The two word-address domains the fold works in, kept apart by type.
//!
//! `ram` and `decoded` are keyed by a word address: a byte address divided
//! by four, with the RAM region's base still on it. The fold captures `ram`
//! as one positionally indexed array, so inside the fold a word is named by
//! its index relative to that base instead. The two differ by
//! `RAM_BASE >> 2`, which is 536,870,912, and both are `u32`.
//!
//! Passing one where the other belongs is silent: an index too large by the
//! base saturates a clamp, and a bound too large by the base makes a
//! comparison that can never be true. [`Widx`] and [`WordAddr`] are
//! separate types so a caller has to say which it means, and the conversion
//! names the base it converts against.

use clickdoom_spec::RAM_BASE;

/// A word address in `ram` and `decoded`'s own key domain, base included.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct WordAddr(u32);

/// A word index relative to the RAM region's base: what subscripts the
/// fold's captured `ram` array, and the domain the text bounds are compared
/// in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Widx(u32);

/// Why a word address could not be rebased.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("word address {addr} is below the base {base}, so it has no index relative to it")]
pub struct BelowBase {
    pub addr: u32,
    pub base: u32,
}

impl WordAddr {
    pub const fn new(word_addr: u32) -> Self {
        Self(word_addr)
    }

    /// The word a byte address falls in.
    pub const fn of_byte(byte_addr: u32) -> Self {
        Self(byte_addr >> 2)
    }

    /// `RAM_BASE`'s own word address, which is the base every conversion
    /// here is against for a real run.
    pub const fn ram_base() -> Self {
        Self(RAM_BASE >> 2)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// This address as an index relative to `base`.
    pub const fn widx_from(self, base: Self) -> Result<Widx, BelowBase> {
        if self.0 < base.0 {
            return Err(BelowBase {
                addr: self.0,
                base: base.0,
            });
        }
        Ok(Widx(self.0 - base.0))
    }
}

impl Widx {
    pub const fn new(widx: u32) -> Self {
        Self(widx)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// This index as an address in `base`'s domain.
    pub const fn word_addr(self, base: WordAddr) -> WordAddr {
        WordAddr(base.0 + self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The distance the two domains are apart. Every bug below is this
    /// number applied once too often or once too few.
    const RAM_BASE_WORD: u32 = RAM_BASE >> 2;

    #[test]
    fn the_two_domains_are_a_whole_ram_base_apart() {
        assert_eq!(WordAddr::ram_base().get(), RAM_BASE_WORD);
        assert_eq!(RAM_BASE_WORD, 536_870_912);
        assert_eq!(
            Widx::new(0).word_addr(WordAddr::ram_base()).get(),
            RAM_BASE_WORD
        );
        assert_eq!(
            WordAddr::new(RAM_BASE_WORD)
                .widx_from(WordAddr::ram_base())
                .unwrap(),
            Widx::new(0)
        );
    }

    /// A relative index used against an absolute-addressed `ram` reads the
    /// wrong word, and the wrong word is a real one, so nothing faults.
    /// This is what a run does when it rebases, and the round trip is what
    /// says the two directions agree.
    #[test]
    fn rebasing_round_trips_for_every_word_of_the_region() {
        for widx in [0u32, 1, 4_095, 98_952, 6_291_455] {
            let relative = Widx::new(widx);
            let absolute = relative.word_addr(WordAddr::ram_base());
            assert_eq!(absolute.get(), RAM_BASE_WORD + widx);
            assert_eq!(
                absolute.widx_from(WordAddr::ram_base()).unwrap(),
                relative,
                "widx {widx} did not survive the round trip"
            );
        }
    }

    /// A write-log entry flushed into `ram` without its base lands about
    /// 536 million words below the image, where it sorts ahead of every
    /// real row rather than replacing one.
    #[test]
    fn an_index_flushed_without_its_base_lands_below_the_image() {
        let stored = Widx::new(1_024);
        let wrong = WordAddr::new(stored.get());
        let right = stored.word_addr(WordAddr::ram_base());
        assert!(wrong < WordAddr::ram_base());
        assert_eq!(right.get() - wrong.get(), RAM_BASE_WORD);
    }

    /// A manifest's absolute bound passed where a relative one belongs is
    /// larger than the whole region, so a comparison against it is
    /// algebraically incapable of being true. The conversion is the only
    /// way to get from one to the other, and it names the base.
    #[test]
    fn an_absolute_bound_is_outside_the_region_it_would_bound() {
        let ram_words = clickdoom_spec::RAM_SIZE / 4;
        let text_end = WordAddr::new(536_969_865);
        assert!(
            text_end.get() > ram_words,
            "the absolute bound is past every index the region has"
        );
        let relative = text_end.widx_from(WordAddr::ram_base()).unwrap();
        assert!(relative.get() < ram_words);
        assert_eq!(relative, Widx::new(98_953));
    }

    #[test]
    fn an_address_below_the_base_has_no_index_relative_to_it() {
        let below = WordAddr::of_byte(clickdoom_spec::FRAMEBUFFER_BASE);
        assert_eq!(
            below.widx_from(WordAddr::ram_base()),
            Err(BelowBase {
                addr: below.get(),
                base: RAM_BASE_WORD,
            })
        );
    }
}
