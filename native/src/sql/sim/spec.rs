//! The world's specials, from `p_spec.c`.

use crate::sql::Statement;

/// `m_fixed.h`: how far a scrolling wall moves in one tic.
const FRACUNIT: i64 = 1 << 16;

/// `p_spec.c`: the line special that scrolls its first side left.
const SCROLL_LEFT: i32 = 48;

/// What `P_InitPicAnims` stops the load for.
///
/// A cycle needs at least two pictures, and both ends have to resolve.
/// `animdefs` names cycles for every episode, so an entry whose first
/// picture this WAD does not carry is skipped rather than an error, and
/// only an entry that starts is required to finish.
pub fn guards(db: &str) -> Vec<Statement> {
    let known = |table: &str, name: &str| {
        format!("(SELECT count() FROM {db}.{table} WHERE upper(name) = upper(a.{name}))")
    };
    let texture = |name: &str| known("tex_textures", name);
    let flat = |name: &str| known("flats", name);
    let number = |table: &str, name: &str| {
        format!("(SELECT min(id) FROM {db}.{table} WHERE upper(name) = upper(a.{name}))")
    };
    vec![Statement::sql(format!(
        "SELECT throwIf(count() > 0, 'P_InitPicAnims: bad cycle')\n\
         FROM {db}.animdefs AS a\n\
         WHERE a.istexture != -1\n\
         AND if(a.istexture != 0, {}, {}) > 0\n\
         AND if(a.istexture != 0, {} - {}, {} - {}) < 1",
        texture("startname"),
        flat("startname"),
        number("tex_textures", "endname"),
        number("tex_textures", "startname"),
        number("flats", "endname"),
        number("flats", "startname"),
    ))]
}

/// `P_UpdateSpecials`: the animated pictures and the scrolling walls.
///
/// It runs before `leveltime` is bumped, so the frame of a cycle comes
/// from the tic before this one.
pub fn update_specials(state: &super::State, db: &str) -> Vec<(String, String)> {
    let mut bindings = anims(db);
    bindings.push(("scroll_lines".to_owned(), scroll_lines(db)));
    let _ = state;
    bindings.extend([
        (
            "now_texturetranslation".to_owned(),
            translation("1", &state.get("texturetranslation")),
        ),
        (
            "now_flattranslation".to_owned(),
            translation("0", &state.get("flattranslation")),
        ),
        ("now_side_textureoffset".to_owned(), scroll(state)),
    ]);
    bindings
}

/// `P_InitPicAnims`' table, as constant arrays.
///
/// `animdefs` names the first and last picture of each cycle by name, and
/// an entry whose first picture this WAD does not carry is left out, which
/// is how one table serves every episode.
fn anims(db: &str) -> Vec<(String, String)> {
    let numbers = |table: &str| {
        format!(
            "(SELECT mapFromArrays(groupArray(upper(name)), groupArray(toInt32(id) + 1))\
             \n     FROM {db}.{table})"
        )
    };
    // A picture number, plus one so that a name no picture carries is 0.
    let picture =
        |name: &str| format!("if(istexture != 0, texnum[upper({name})], flatnum[upper({name})])");
    let start = picture("startname");
    let end = picture("endname");
    let kept = format!("istexture != -1 AND {start} != 0");
    let column = |expr: &str| {
        format!(
            "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((id, {expr}))))\
             \n     FROM {db}.animdefs WHERE {kept})"
        )
    };
    vec![
        ("texnum".to_owned(), numbers("tex_textures")),
        ("flatnum".to_owned(), numbers("flats")),
        (
            "anim_istexture".to_owned(),
            column("toInt32(istexture != 0)"),
        ),
        (
            "anim_basepic".to_owned(),
            column(&format!("toInt32({start} - 1)")),
        ),
        (
            "anim_numpics".to_owned(),
            column(&format!("toInt32({end} - {start} + 1)")),
        ),
        ("anim_speed".to_owned(), column("toInt32(speed)")),
    ]
}

/// One translation table: each picture is itself unless a cycle covers it,
/// and then it is the frame of that cycle this tic shows.
///
/// `carried` drives the map so the table keeps its length without the
/// picture count being named again; every entry is a function of the index.
fn translation(istexture: &str, carried: &str) -> String {
    let covers = format!(
        "anim_istexture[a] = {istexture} AND i >= anim_basepic[a] \
         AND i < anim_basepic[a] + anim_numpics[a]"
    );
    let at = format!("arrayFirstIndex(a -> {covers}, arrayEnumerate(anim_istexture))");
    format!(
        "arrayMap((was, i) -> toInt32(if({at} = 0, i, \
         anim_basepic[{at}] + ((intDiv(prev_leveltime, anim_speed[{at}]) + i) \
         % anim_numpics[{at}]))), {carried}, \
         arrayMap(n -> toInt32(n) - 1, arrayEnumerate({carried})))"
    )
}

/// The scrolling walls: every side named as the first side of a line whose
/// special still scrolls moves one unit.
///
/// `P_SpawnSpecials` fixed the list of lines at load and
/// `P_UpdateSpecials` reads each line's special again, so a line that has
/// stopped scrolling stays in the list and does nothing.
fn scroll(state: &super::State) -> String {
    let special = state.get("line_special");
    let carried = state.get("side_textureoffset");
    let active = format!(
        "arrayMap(l -> line_side0[1 + l], \
         arrayFilter(l -> {special}[1 + l] = {SCROLL_LEFT}, scroll_lines))"
    );
    format!(
        "arrayMap((was, s) -> toInt32(was + {FRACUNIT} * countEqual({active}, toInt32(s) - 1)), \
         {carried}, arrayEnumerate({carried}))"
    )
}

/// The lines `P_SpawnSpecials` put on the scrolling list, in line order.
fn scroll_lines(db: &str) -> String {
    format!(
        "(SELECT arrayMap(t -> t.2, arraySort(t -> t.1, groupArray((id, toInt32(id)))))\
         \n     FROM {db}.lv_lines WHERE special = {SCROLL_LEFT})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_animation_reads_the_tic_before_it() {
        let text = translation("1", "prev_texturetranslation");
        assert!(text.contains("intDiv(prev_leveltime, anim_speed"));
        assert!(text.contains("arrayEnumerate(prev_texturetranslation)"));
    }

    #[test]
    fn a_scroller_moves_one_unit_a_tic() {
        let text = scroll(&super::super::State::default());
        assert!(text.contains("65536 * countEqual"));
        assert!(text.contains(&format!("prev_line_special[1 + l] = {SCROLL_LEFT}")));
    }

    #[test]
    fn every_builder_balances_its_parentheses() {
        let mut texts: Vec<String> = update_specials(&super::super::State::default(), "nat")
            .into_iter()
            .map(|(_, expr)| expr)
            .collect();
        texts.extend(guards("nat").into_iter().map(|s| s.sql));
        for text in texts {
            let depth = text.chars().fold(0i32, |d, c| match c {
                '(' => d + 1,
                ')' => d - 1,
                _ => d,
            });
            assert_eq!(depth, 0, "{text}");
        }
    }
}
