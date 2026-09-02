//! Where each engine field sits, read from the table the toolchain emits.
//!
//! `DEVELOPING.md` says how `refemu/probe/layout.tsv` is produced and what
//! gates it. This module parses it and resolves it into the offsets the probe
//! reads, checking each field's width against the width the probe will read it
//! at. A table whose `sector_t.floorheight` is two bytes wide is not the table
//! this code was written against, and a run against it would report plausible
//! nonsense.

use std::collections::HashMap;

use super::ProbeError;

/// One row of the table.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Field {
    pub offset: u32,
    pub size: u32,
}

/// An array field: where it starts and how many elements it holds.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct ArrayField {
    pub offset: u32,
    pub count: u32,
}

/// The whole table, by struct and field name.
#[derive(Debug)]
pub struct Layout {
    fields: HashMap<(String, String), Field>,
}

/// The field name a struct's own size is filed under.
const SIZEOF: &str = "sizeof";

impl Layout {
    /// Parses the table. Every row is `struct field offset size`, tab
    /// separated, and a line starting with `#` is a comment.
    pub fn parse(text: &str) -> Result<Self, ProbeError> {
        let mut fields = HashMap::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let row = |what: &'static str| ProbeError::LayoutRow {
                line: number + 1,
                what,
            };
            let mut columns = line.split('\t');
            let mut next = |what| columns.next().ok_or_else(|| row(what));
            let type_name = next("a struct name")?.to_owned();
            let field_name = next("a field name")?.to_owned();
            let offset = next("an offset")?
                .parse()
                .map_err(|_| row("a decimal offset"))?;
            let size = next("a size")?.parse().map_err(|_| row("a decimal size"))?;
            if columns.next().is_some() {
                return Err(row("four columns"));
            }
            if fields
                .insert((type_name, field_name), Field { offset, size })
                .is_some()
            {
                return Err(row("each field once"));
            }
        }
        if fields.is_empty() {
            return Err(ProbeError::LayoutEmpty);
        }
        Ok(Self { fields })
    }

    /// The offset of a field the probe reads at `width` bytes.
    ///
    /// The width is the check: a table that disagrees with what this code
    /// reads is refused before the run rather than producing numbers taken
    /// from the wrong bytes.
    pub fn field(&self, type_name: &str, field: &str, width: u32) -> Result<u32, ProbeError> {
        let found = self.lookup(type_name, field)?;
        if found.size != width {
            return Err(ProbeError::LayoutWidth {
                type_name: type_name.to_owned(),
                field: field.to_owned(),
                want: width,
                got: found.size,
            });
        }
        Ok(found.offset)
    }

    /// The offset of a field whose width the probe does not fix, and the width
    /// the table gives it. Arrays use this: the extent is how many elements
    /// there are.
    pub fn array(
        &self,
        type_name: &str,
        field: &str,
        element: u32,
    ) -> Result<(u32, u32), ProbeError> {
        let found = self.lookup(type_name, field)?;
        if element == 0 || !found.size.is_multiple_of(element) {
            return Err(ProbeError::LayoutWidth {
                type_name: type_name.to_owned(),
                field: field.to_owned(),
                want: element,
                got: found.size,
            });
        }
        Ok((found.offset, found.size / element))
    }

    /// A struct's own size, which is the stride of an array of them.
    pub fn size_of(&self, type_name: &str) -> Result<u32, ProbeError> {
        Ok(self.lookup(type_name, SIZEOF)?.size)
    }

    fn lookup(&self, type_name: &str, field: &str) -> Result<Field, ProbeError> {
        self.fields
            .get(&(type_name.to_owned(), field.to_owned()))
            .copied()
            .ok_or_else(|| ProbeError::LayoutMissing {
                type_name: type_name.to_owned(),
                field: field.to_owned(),
            })
    }
}

/// Declares the offsets one struct contributes, resolved once from the table.
///
/// Each entry names the width the probe reads the field at, so resolving is
/// also the check that the table agrees. `[N]` is an array of `N`-byte
/// elements and resolves to an `ArrayField`, whose count comes from the
/// field's extent. `field as "name"` spells a C field name that is not a Rust
/// identifier.
macro_rules! offsets {
    (
        $(#[$meta:meta])*
        $name:ident from $type_name:literal {
            $($field:ident $(as $c_name:literal)? : $width:tt,)*
        }
    ) => {
        $(#[$meta])*
        pub struct $name {
            /// The struct's own size, which is the stride of an array of them.
            pub size: u32,
            $(pub $field: offsets!(@type $width),)*
        }

        impl $name {
            pub fn resolve(layout: &$crate::probe::layout::Layout)
                -> Result<Self, $crate::probe::ProbeError>
            {
                Ok(Self {
                    size: layout.size_of($type_name)?,
                    $($field: offsets!(
                        @resolve layout, $type_name,
                        offsets!(@name $field $(, $c_name)?), $width
                    )?,)*
                })
            }
        }
    };
    (@type [$element:literal]) => { $crate::probe::layout::ArrayField };
    (@type $width:literal) => { u32 };
    (@resolve $layout:expr, $type_name:literal, $field:expr, [$element:literal]) => {
        $layout.array($type_name, $field, $element).map(|(offset, count)| {
            $crate::probe::layout::ArrayField { offset, count }
        })
    };
    (@resolve $layout:expr, $type_name:literal, $field:expr, $width:literal) => {
        $layout.field($type_name, $field, $width)
    };
    (@name $field:ident) => { stringify!($field) };
    (@name $field:ident, $c_name:literal) => { $c_name };
}

pub(crate) use offsets;

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
# clickdoom-layout 1
# columns\tstruct\tfield\toffset\tsize
thinker_t\tsizeof\t0\t12
thinker_t\tprev\t0\t4
thinker_t\tnext\t4\t4
mapthing_t\tx\t0\t2
player_t\tsizeof\t0\t288
player_t\tpowers\t44\t24
";

    #[test]
    fn a_field_resolves_to_its_offset_at_the_width_it_is_read_at() {
        let layout = Layout::parse(TABLE).unwrap();
        assert_eq!(layout.size_of("thinker_t").unwrap(), 12);
        assert_eq!(layout.field("thinker_t", "next", 4).unwrap(), 4);
        assert_eq!(layout.field("mapthing_t", "x", 2).unwrap(), 0);
    }

    #[test]
    fn a_size_that_disagrees_with_the_width_read_is_refused() {
        let layout = Layout::parse(TABLE).unwrap();
        let err = layout.field("mapthing_t", "x", 4).unwrap_err();
        assert!(
            matches!(
                err,
                ProbeError::LayoutWidth {
                    want: 4,
                    got: 2,
                    ..
                }
            ),
            "{err}"
        );
        assert!(err.to_string().contains("mapthing_t.x"), "{err}");
    }

    #[test]
    fn an_array_field_reports_how_many_elements_it_holds() {
        let layout = Layout::parse(TABLE).unwrap();
        assert_eq!(layout.array("player_t", "powers", 4).unwrap(), (44, 6));
        // An extent that is not a whole number of elements is the same kind of
        // disagreement as a wrong width.
        assert!(layout.array("player_t", "powers", 5).is_err());
        assert!(layout.array("player_t", "powers", 0).is_err());
    }

    #[test]
    fn a_field_the_table_does_not_carry_is_named_in_the_error() {
        let layout = Layout::parse(TABLE).unwrap();
        let err = layout.field("thinker_t", "function", 4).unwrap_err();
        assert!(err.to_string().contains("thinker_t.function"), "{err}");
    }

    #[test]
    fn a_malformed_table_is_an_error_naming_the_line() {
        for (text, at) in [
            ("a\tb\t0\t4\nx\ty\n", 2),
            ("a\tb\t0\tnope\n", 1),
            ("a\tb\t0\t4\ta\tb\n", 1),
            // The same field twice: one of them is wrong and nothing says
            // which.
            ("a\tb\t0\t4\na\tb\t8\t4\n", 2),
        ] {
            let err = Layout::parse(text).unwrap_err();
            assert!(
                matches!(err, ProbeError::LayoutRow { line, .. } if line == at),
                "{text:?}: {err}"
            );
        }
        assert!(matches!(
            Layout::parse("# nothing but a comment\n"),
            Err(ProbeError::LayoutEmpty)
        ));
    }

    offsets! {
        /// A struct declared the way the probe declares its own.
        TestOffsets from "thinker_t" {
            prev: 4,
            next_link as "next": 4,
        }
    }

    offsets! {
        TestArrays from "player_t" {
            powers: [4],
        }
    }

    #[test]
    fn a_declared_struct_resolves_every_field_it_names() {
        let layout = Layout::parse(TABLE).unwrap();
        let offsets = TestOffsets::resolve(&layout).unwrap();
        assert_eq!(offsets.size, 12);
        assert_eq!(offsets.prev, 0);
        assert_eq!(offsets.next_link, 4);
    }

    #[test]
    fn a_declared_array_field_carries_its_element_count() {
        let layout = Layout::parse(TABLE).unwrap();
        let offsets = TestArrays::resolve(&layout).unwrap();
        assert_eq!(offsets.size, 288);
        assert_eq!(
            offsets.powers,
            ArrayField {
                offset: 44,
                count: 6
            }
        );
    }
}
