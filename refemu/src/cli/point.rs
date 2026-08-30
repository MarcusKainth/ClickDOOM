//! Where a run starts watching, and where it stops.
//!
//! One grammar, three subsets. Every flag that names a position in a run
//! spells it the same way, and each says which kinds it accepts, so a
//! misplaced one is a usage error naming the alternatives rather than a
//! silently different run.

use std::fmt;
use std::str::FromStr;

/// A position in a run.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Point {
    /// Before the first instruction.
    Start,
    /// Once this many instructions have retired.
    Icount(u64),
    /// Once the frame at this index has retired. The index counts announced
    /// frames from zero, and is not the number the program writes.
    Frame(u64),
    /// Wherever the run ends.
    End,
    /// When the machine halts.
    Halt,
    /// When the instruction budget runs out.
    Budget,
}

impl Point {
    const fn kind(self) -> &'static str {
        match self {
            Point::Start => "start",
            Point::Icount(_) => "icount:N",
            Point::Frame(_) => "frame:N",
            Point::End => "end",
            Point::Halt => "halt",
            Point::Budget => "budget",
        }
    }

    fn parse_in(text: &str, allowed: &[&str]) -> Result<Point, String> {
        let point = match text {
            "start" => Point::Start,
            "end" => Point::End,
            "halt" => Point::Halt,
            "budget" => Point::Budget,
            _ => match text.split_once(':') {
                Some(("icount", n)) => Point::Icount(parse_count(n)?),
                Some(("frame", n)) => Point::Frame(parse_count(n)?),
                Some((kind, _)) => {
                    return Err(format!(
                        "`{kind}` is not a position. Write one of {}",
                        allowed.join(", ")
                    ));
                }
                None => {
                    return Err(format!(
                        "`{text}` is not a position. Write one of {}",
                        allowed.join(", ")
                    ));
                }
            },
        };
        if !allowed.contains(&point.kind()) {
            return Err(format!(
                "`{text}` is not allowed here. Write one of {}",
                allowed.join(", ")
            ));
        }
        Ok(point)
    }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Point::Icount(n) => write!(f, "icount:{n}"),
            Point::Frame(n) => write!(f, "frame:{n}"),
            other => f.write_str(other.kind()),
        }
    }
}

/// Where a run stops.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct StopAt(pub Point);

impl FromStr for StopAt {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Point::parse_in(text, &["icount:N", "frame:N", "halt", "budget"]).map(StopAt)
    }
}

impl fmt::Display for StopAt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Where a run starts recording.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct WatchFrom(pub Point);

impl FromStr for WatchFrom {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Point::parse_in(text, &["icount:N", "frame:N", "start"]).map(WatchFrom)
    }
}

impl fmt::Display for WatchFrom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A count, decimal or hexadecimal, with `_` allowed as a separator.
pub fn parse_count(text: &str) -> Result<u64, String> {
    let cleaned = text.replace('_', "");
    let parsed = match cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => cleaned.parse::<u64>(),
    };
    parsed.map_err(|_| format!("`{text}` is not a count"))
}

/// An address, decimal or hexadecimal.
pub fn parse_addr(text: &str) -> Result<u32, String> {
    let value = parse_count(text)?;
    u32::try_from(value).map_err(|_| format!("`{text}` does not fit in 32 bits"))
}

/// A sixteen-digit hash, as the trace spells it.
pub fn parse_hash64(text: &str) -> Result<u64, String> {
    let cleaned = text.strip_prefix("0x").unwrap_or(text);
    u64::from_str_radix(cleaned, 16).map_err(|_| format!("`{text}` is not a 64-bit hex hash"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_takes_the_positions_a_run_can_end_at() {
        assert_eq!("halt".parse(), Ok(StopAt(Point::Halt)));
        assert_eq!("budget".parse(), Ok(StopAt(Point::Budget)));
        assert_eq!("icount:100".parse(), Ok(StopAt(Point::Icount(100))));
        assert_eq!("frame:0".parse(), Ok(StopAt(Point::Frame(0))));
    }

    #[test]
    fn a_stop_refuses_a_position_a_run_cannot_end_at() {
        for text in ["start", "end"] {
            let err = text.parse::<StopAt>().unwrap_err();
            assert!(err.contains("not allowed here"), "{err}");
            assert!(err.contains("icount:N"), "{err}");
        }
    }

    #[test]
    fn a_watch_point_refuses_an_ending_rather_than_a_position() {
        assert_eq!("start".parse(), Ok(WatchFrom(Point::Start)));
        assert_eq!("icount:5".parse(), Ok(WatchFrom(Point::Icount(5))));
        assert!("halt".parse::<WatchFrom>().is_err());
        assert!("budget".parse::<WatchFrom>().is_err());
    }

    #[test]
    fn an_unknown_kind_names_what_is_allowed() {
        let err = "tick:5".parse::<StopAt>().unwrap_err();
        assert!(err.contains("`tick` is not a position"), "{err}");
        assert!(err.contains("frame:N"), "{err}");
        let err = "5".parse::<StopAt>().unwrap_err();
        assert!(err.contains("`5` is not a position"), "{err}");
    }

    #[test]
    fn a_position_round_trips_through_its_own_spelling() {
        for text in ["halt", "budget", "icount:100", "frame:7"] {
            assert_eq!(text.parse::<StopAt>().unwrap().to_string(), text);
        }
    }

    #[test]
    fn counts_take_decimal_hex_and_separators() {
        assert_eq!(parse_count("100"), Ok(100));
        assert_eq!(parse_count("1_048_576"), Ok(1_048_576));
        assert_eq!(parse_count("0x10"), Ok(16));
        assert_eq!(parse_count("0X10"), Ok(16));
        assert!(parse_count("ten").is_err());
        assert!(parse_count("-1").is_err());
    }

    #[test]
    fn an_address_has_to_fit_in_the_machine() {
        assert_eq!(parse_addr("0x80000000"), Ok(0x8000_0000));
        assert_eq!(parse_addr("2147483648"), Ok(0x8000_0000));
        assert!(parse_addr("0x1_0000_0000").is_err());
    }

    #[test]
    fn a_hash_parses_with_or_without_a_prefix() {
        assert_eq!(parse_hash64("fe5d82c0f42d45f1"), Ok(0xfe5d_82c0_f42d_45f1));
        assert_eq!(parse_hash64("0xff"), Ok(255));
        assert!(parse_hash64("nothex").is_err());
    }
}
