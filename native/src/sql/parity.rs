//! Where this simulation and the reference emulator disagree.
//!
//! Two questions get asked, and each has its own comparison. A rendered
//! frame is compared pixel by pixel against a `refemu` frame dump loaded
//! into a table beside `native_frames`. A tic's state row is compared field
//! by field against the probe rows in `probe_state`, joined on `gametic`.
//!
//! Five state columns are dropped from the field comparison.
//! `refemu/probe/README.md` says why the probe cannot read an identity, and
//! comparing by slot is what stands in for it.

use clickdoom_spec::native_state::{
    ANIM_FIELDS, BUTTON_FIELDS, GAME_FIELDS, HUD_FIELDS, INPUT_FIELDS, LINE_SIDE_FIELDS,
    MOBJ_FIELDS, PLAYER_FIELDS, PSPRITE_FIELDS, SECTOR_FIELDS, SECTOR_THINKER_FIELDS,
    sector_thinker_kind,
};

/// Columns a thinker's identity would fill, which nothing in the engine's
/// memory carries.
const IDENTITIES: [&str; 5] = ["m_id", "s_seq", "next_seq", "next_linkseq", "m_linkseq"];

/// What the probe's copy of a column is called in the joined row. No
/// contract field starts with this, so the two sides never collide.
const THEIRS: &str = "probe__";

/// The first field that differs, earliest tic first.
///
/// One row, or none when every compared tic agrees.
pub fn first_divergence(db: &str) -> String {
    format!(
        "SELECT tic, d.2 AS kind, d.4 AS slot, d.3 AS field, d.5 AS ours, d.6 AS theirs\n\
         FROM\n(\n{}\n)\nWHERE d.7 = 1\nORDER BY tic ASC, d.1 ASC\nLIMIT 1",
        indent(&comparison(db))
    )
}

/// Every field that ever differs, with the tic it first does and the values
/// there. Earliest tic first, then the contract's order, so the summary
/// reads the way the first divergence does.
pub fn field_summary(db: &str) -> String {
    format!(
        "SELECT\n    \
         d.3 AS field,\n    \
         any(d.2) AS kind,\n    \
         count() AS tics,\n    \
         min(tic) AS first_tic,\n    \
         argMin(d.4, tic) AS slot,\n    \
         argMin(d.5, tic) AS ours,\n    \
         argMin(d.6, tic) AS theirs\n\
         FROM\n(\n{}\n)\nWHERE d.7 = 1\nGROUP BY field\n\
         ORDER BY first_tic ASC, any(d.1) ASC",
        indent(&comparison(db))
    )
}

/// One row per `(tic, compared field)`: the field's position in the
/// contract, its group, its name, the slot, both values, and whether they
/// differ.
fn comparison(db: &str) -> String {
    let verdicts: Vec<String> = compared().iter().enumerate().map(verdict).collect();
    format!(
        "SELECT tic, arrayJoin([\n{}\n]) AS d\nFROM\n(\n{}\n)",
        verdicts.join(",\n"),
        indent(&joined(db))
    )
}

/// A field's verdict for one tic, as the tuple `comparison` emits.
///
/// An array field compares element by element, and an element the field's
/// mask names is left out on both sides.
fn verdict((at, field): (usize, &Field)) -> String {
    let ours = field.name;
    let theirs = format!("{THEIRS}{}", field.name);
    let mask = field.mask.as_deref();
    let (slot, ours, theirs, differs) = if field.array {
        let differing = differing(ours, &theirs, mask);
        (
            format!(
                "toUInt32(if(length({ours}) != length({theirs}), 0, \
                 indexOf({differing}, 1)))"
            ),
            element(ours, &theirs, ours, mask),
            element(ours, &theirs, &theirs, mask),
            format!("toUInt8(length({ours}) != length({theirs}) OR has({differing}, 1))"),
        )
    } else {
        (
            "toUInt32(0)".to_owned(),
            format!("toString({ours})"),
            format!("toString({theirs})"),
            format!("toUInt8({ours} != {theirs})"),
        )
    };
    format!(
        "    (toUInt32({at}), '{}', '{}', {slot}, {ours}, {theirs}, {differs})",
        field.kind, field.name
    )
}

/// One flag per element: 1 where the two sides differ and the mask does
/// not name the element.
fn differing(ours: &str, theirs: &str, mask: Option<&str>) -> String {
    match mask {
        Some(mask) => format!(
            "arrayMap((a, b, k) -> toUInt8(a != b AND NOT ({mask})), \
             {ours}, {theirs}, arrayEnumerate({ours}))"
        ),
        None => format!("arrayMap((a, b) -> toUInt8(a != b), {ours}, {theirs})"),
    }
}

/// The element of `side` the slot names, or both lengths when the two
/// arrays are not the same length.
fn element(ours: &str, theirs: &str, side: &str, mask: Option<&str>) -> String {
    format!(
        "if(length({ours}) != length({theirs}), \
         concat('length ', toString(length({side}))), \
         toString({side}[indexOf({}, 1)]))",
        differing(ours, theirs, mask)
    )
}

/// `T_VerticalDoor` never reads `topcountdown` while the door goes up, and
/// the engine leaves it as the zone allocator returned it until the door
/// reaches the top and writes it. So a door's count is compared only when
/// its direction is not up. `k` is the thinker's slot, one-based.
fn door_count_mask() -> String {
    format!(
        "s_kind[k] = {} AND s_direction[k] = 1",
        sector_thinker_kind::DOOR
    )
}

/// `native_state` beside the last `probe_state` row of each `gametic`.
///
/// The melt commits many frames within one tic and the state does not move
/// between them, so the last row of a `gametic` is the state that tic left.
fn joined(db: &str) -> String {
    let theirs: Vec<String> = compared()
        .iter()
        .map(|field| {
            format!(
                "        argMax({}, frame_index) AS {THEIRS}{}",
                field.name, field.name
            )
        })
        .collect();
    format!(
        "SELECT *\nFROM\n(\n    \
         SELECT\n        gametic AS tic,\n{}\n    \
         FROM {db}.probe_state\n    GROUP BY gametic\n) AS p\n\
         INNER JOIN (SELECT * FROM {db}.native_state) AS s ON s.tic = p.tic",
        theirs.join(",\n")
    )
}

/// A field the comparison covers.
struct Field {
    name: &'static str,
    kind: &'static str,
    array: bool,
    /// An expression over the slot `k` naming elements left out of the
    /// comparison.
    mask: Option<String>,
}

/// Every contract field but the identities, each with its group and whether
/// it is an array column.
fn compared() -> Vec<Field> {
    let types = super::native_state_types();
    let groups: [(&'static str, &[&'static str]); 11] = [
        ("game", GAME_FIELDS),
        ("mobj", MOBJ_FIELDS),
        ("sector_thinker", SECTOR_THINKER_FIELDS),
        ("sector", SECTOR_FIELDS),
        ("line_side", LINE_SIDE_FIELDS),
        ("button", BUTTON_FIELDS),
        ("anim", ANIM_FIELDS),
        ("player", PLAYER_FIELDS),
        ("psprite", PSPRITE_FIELDS),
        ("hud", HUD_FIELDS),
        ("input", INPUT_FIELDS),
    ];
    let mut fields = Vec::new();
    for (kind, names) in groups {
        for name in names {
            if IDENTITIES.contains(name) {
                continue;
            }
            let declared = types
                .iter()
                .find(|(column, _)| column == name)
                .map(|(_, kind)| *kind)
                .unwrap_or_else(|| panic!("native_state declares no column {name}"));
            fields.push(Field {
                name,
                kind,
                array: declared.starts_with("Array("),
                mask: match *name {
                    "s_count" => Some(door_count_mask()),
                    _ => None,
                },
            });
        }
    }
    fields
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first pixel where `native_frames.fb` for `frame` differs from the
/// reference in `{db}.ref_frames`, and how many differ in all.
///
/// A row comes back whatever the answer: `differing` is 0 when the frame
/// matches, and the coordinates are then meaningless. `region` restricts the
/// comparison to part of the screen, so a caller can ask about the view
/// without the status bar.
pub fn first_difference(db: &str, region: Region) -> String {
    let rows = region.rows();
    format!(
        "WITH \
         assumeNotNull((SELECT fb FROM {db}.native_frames WHERE frame = {{frame:UInt32}})) AS mine, \
         assumeNotNull((SELECT fb FROM {db}.ref_frames WHERE frame = {{frame:UInt32}})) AS reference, \
         arrayFilter(i -> substring(mine, i, 1) != substring(reference, i, 1), {rows}) AS bad \
         SELECT \
         toUInt64(length(bad)) AS differing, \
         toInt32(if(empty(bad), -1, (bad[1] - 1) % 320)) AS x, \
         toInt32(if(empty(bad), -1, intDiv(bad[1] - 1, 320))) AS y, \
         toInt32(if(empty(bad), -1, reinterpretAsUInt8(substring(mine, bad[1], 1)))) AS ours, \
         toInt32(if(empty(bad), -1, reinterpretAsUInt8(substring(reference, bad[1], 1)))) AS theirs"
    )
}

/// Which pixels a comparison looks at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Region {
    /// The whole 320x200 framebuffer.
    Screen,
    /// The 320x168 view the renderer draws into.
    View,
    /// The 320x32 status bar under it.
    StatusBar,
}

impl Region {
    /// The one-based framebuffer offsets the region covers.
    fn rows(self) -> &'static str {
        match self {
            Region::Screen => "range(1, 64001)",
            Region::View => "range(1, 53761)",
            Region::StatusBar => "range(53761, 64001)",
        }
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;
    use clickdoom_spec::native_state::all_fields;

    #[test]
    fn every_field_but_the_identities_is_compared() {
        let compared = compared();
        assert_eq!(compared.len(), all_fields().len() - IDENTITIES.len());
        for identity in IDENTITIES {
            assert!(!compared.iter().any(|f| f.name == identity), "{identity}");
        }
        assert!(compared.iter().any(|f| f.name == "leveltime" && !f.array));
        assert!(compared.iter().any(|f| f.name == "m_x" && f.array));
    }

    /// The mask leaves out a door's count while the door goes up, and
    /// nothing else.
    #[test]
    fn only_a_rising_doors_count_is_left_out() {
        let masked: Vec<&str> = compared()
            .iter()
            .filter(|f| f.mask.is_some())
            .map(|f| f.name)
            .collect();
        assert_eq!(masked, ["s_count"]);
        let sql = first_divergence("nat");
        assert_eq!(
            sql.matches("s_direction[k] = 1").count(),
            4,
            "the slot, both elements and the verdict"
        );
    }

    #[test]
    fn the_first_divergence_names_the_columns_the_report_carries() {
        let sql = first_divergence("nat");
        for column in ["tic", "kind", "slot", "field", "ours", "theirs"] {
            assert!(sql.contains(&format!("AS {column}")), "{column}");
        }
        assert!(sql.ends_with("LIMIT 1"));
        assert!(sql.contains("nat.probe_state"));
        assert!(sql.contains("nat.native_state"));
    }

    #[test]
    fn an_identity_column_appears_nowhere_in_either_query() {
        for sql in [first_divergence("nat"), field_summary("nat")] {
            for identity in IDENTITIES {
                assert!(!sql.contains(identity), "{identity} is still compared");
            }
        }
    }

    #[test]
    fn both_queries_balance_their_parentheses() {
        for sql in [first_divergence("nat"), field_summary("nat")] {
            let depth = sql.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0);
        }
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    fn a_region_covers_the_pixels_it_names() {
        assert!(first_difference("nat", Region::View).contains("range(1, 53761)"));
        assert!(first_difference("nat", Region::StatusBar).contains("range(53761, 64001)"));
    }

    #[test]
    fn the_query_names_the_database_on_both_sides() {
        let sql = first_difference("nat", Region::Screen);
        assert!(sql.contains("nat.native_frames"));
        assert!(sql.contains("nat.ref_frames"));
    }
}
