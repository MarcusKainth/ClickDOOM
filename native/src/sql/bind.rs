//! Binding a value once inside a lambda.
//!
//! A lambda has no `WITH`. Naming a value twice inside one means
//! evaluating it twice and carrying it twice in the query tree, and a
//! chain of such names grows the tree by the product of its branches. A
//! single-element array threads the values through instead: each level
//! binds what the levels after it read into a tuple, and they read it back
//! by position.
//!
//! The levels are a pipeline. Each one maps the level below it to a wider
//! tuple and the body maps the last, so a level's own expressions read the
//! level below through its lambda's parameter and sit one lambda deep
//! whatever the chain's length. A map's array argument is outside its
//! lambda, which is what keeps the levels beside each other rather than
//! inside each other, and what a node costs to analyse and to run grows
//! with the lambda depth it sits at.
//!
//! [`chain`] takes values in dependency order and returns one expression.

/// The lambda parameter each level binds its values into.
///
/// A chain written inside another chain's body has to take a `prefix` of
/// its own. The outer chain rewrites its value names wherever they appear,
/// the inner lambda included, so a parameter both share would shadow the
/// outer one at the point the inner body reads it.
fn level_name(prefix: &str, at: usize) -> String {
    format!("{prefix}{at}")
}

/// `values` bound in order, ending in `body`.
///
/// A value may name the ones before it. Two things keep the chain cheap.
/// A value named once is written into the one place that reads it, so it
/// costs no level. The rest sit at the shallowest level their own
/// dependencies allow, because a level's tuple is copied into every level
/// above it.
pub fn chain(values: &[(String, String)], body: &str) -> String {
    chain_in("b", values, body)
}

/// [`chain`] with the lambda parameters named `<prefix>0`, `<prefix>1` and
/// so on, for a chain that sits inside another one's body.
pub fn chain_in(prefix: &str, values: &[(String, String)], body: &str) -> String {
    let (values, body) = inline_single_uses(values, body);
    if values.is_empty() {
        return body;
    }
    let levels = levels(&values);
    // What each level's tuple holds, in order: everything bound so far.
    let mut carried: Vec<String> = Vec::new();
    let mut members: Vec<Vec<String>> = Vec::new();
    for (at, level) in levels.iter().enumerate() {
        let mut tuple: Vec<String> = (1..=carried.len())
            .map(|index| format!("{}.{index}", level_name(prefix, at.wrapping_sub(1))))
            .collect();
        for (_, expr) in level {
            tuple.push(resolve(prefix, expr, &carried, at));
        }
        carried.extend(level.iter().map(|(name, _)| name.clone()));
        members.push(tuple);
    }
    // The first level's tuple names nothing bound, so it is the literal
    // the pipeline starts from; every level after it maps the one below.
    let mut sql = format!("[tuple({})]", members[0].join(", "));
    for (at, member) in members.iter().enumerate().skip(1) {
        sql = format!(
            "arrayMap({} -> tuple({}), {sql})",
            level_name(prefix, at - 1),
            member.join(", ")
        );
    }
    format!(
        "arrayMap({} -> {}, {sql})[1]",
        level_name(prefix, levels.len() - 1),
        resolve(prefix, &body, &carried, levels.len())
    )
}

/// A value nothing reads twice is written into the place that reads it.
///
/// Binding one costs a level for no sharing, and a level is expanded into
/// every level above it.
fn inline_single_uses(values: &[(String, String)], body: &str) -> (Vec<(String, String)>, String) {
    let mut values: Vec<(String, String)> = values.to_vec();
    let mut body = body.to_owned();
    let mut at = 0;
    while at < values.len() {
        let (name, expr) = values[at].clone();
        let uses: usize = values[at + 1..]
            .iter()
            .map(|(_, later)| count(later, &name))
            .sum::<usize>()
            + count(&body, &name);
        if uses == 1 {
            let inlined = format!("({expr})");
            for (_, later) in values[at + 1..].iter_mut() {
                *later = rename(later, &name, &inlined);
            }
            body = rename(&body, &name, &inlined);
            values.remove(at);
        } else {
            at += 1;
        }
    }
    (values, body)
}

/// How often `expr` names `binding` as a whole word.
fn count(expr: &str, binding: &str) -> usize {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    expr.match_indices(binding)
        .filter(|(at, _)| {
            let before = expr[..*at].chars().next_back();
            let after = expr[at + binding.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident)
        })
        .count()
}

/// `expr` with every bound name replaced by its place in the tuple the
/// level below `at` holds.
fn resolve(prefix: &str, expr: &str, carried: &[String], at: usize) -> String {
    let mut out = expr.to_owned();
    for (index, name) in carried.iter().enumerate() {
        out = rename(
            &out,
            name,
            &format!("{}.{}", level_name(prefix, at - 1), index + 1),
        );
    }
    out
}

/// The values levelled so that each sits one below the deepest thing it
/// names, which is the shallowest chain its dependencies allow.
fn levels(values: &[(String, String)]) -> Vec<Vec<(String, String)>> {
    let mut depth: Vec<usize> = Vec::with_capacity(values.len());
    for (at, (_, expr)) in values.iter().enumerate() {
        let deepest = values[..at]
            .iter()
            .enumerate()
            .filter(|(_, (earlier, _))| names(expr, earlier))
            .map(|(index, _)| depth[index] + 1)
            .max()
            .unwrap_or(0);
        depth.push(deepest);
    }
    let mut levels: Vec<Vec<(String, String)>> =
        vec![Vec::new(); depth.iter().max().map_or(0, |d| d + 1)];
    for (at, value) in values.iter().enumerate() {
        levels[depth[at]].push(value.clone());
    }
    levels
}

/// Whether `expr` names `binding` as a whole word.
fn names(expr: &str, binding: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    expr.match_indices(binding).any(|(at, _)| {
        let before = expr[..at].chars().next_back();
        let after = expr[at + binding.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// `expr` with every whole-word use of `from` replaced by `to`.
fn rename(expr: &str, from: &str, to: &str) -> String {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(expr.len());
    let mut rest = expr;
    while let Some(at) = rest.find(from) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + from.len()..].chars().next();
        out.push_str(&rest[..at]);
        if before.is_some_and(ident) || after.is_some_and(ident) {
            out.push_str(from);
        } else {
            out.push_str(to);
        }
        rest = &rest[at + from.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, expr)| ((*name).to_owned(), (*expr).to_owned()))
            .collect()
    }

    #[test]
    fn a_value_read_once_is_written_where_it_is_read() {
        let sql = chain(&values(&[("a", "x + 1"), ("c", "x + 2")]), "a + c");
        assert_eq!(sql, "(x + 1) + (x + 2)");
    }

    #[test]
    fn values_read_twice_and_depending_on_nothing_share_a_level() {
        let sql = chain(&values(&[("a", "x + 1"), ("c", "x + 2")]), "a + c + a + c");
        assert_eq!(
            sql,
            "arrayMap(b0 -> b0.1 + b0.2 + b0.1 + b0.2, [tuple(x + 1, x + 2)])[1]"
        );
    }

    #[test]
    fn a_value_that_reads_another_sits_one_level_below_it() {
        let sql = chain(&values(&[("a", "x + 1"), ("b", "a * a")]), "a + b");
        assert_eq!(
            sql,
            "arrayMap(b0 -> b0.1 + (b0.1 * b0.1), [tuple(x + 1)])[1]"
        );
    }

    #[test]
    fn a_value_reading_nothing_bound_stays_at_the_top_level() {
        let sql = chain(
            &values(&[("a", "x + 1"), ("b", "a * a"), ("c", "y + 1")]),
            "a + b + b + c + c",
        );
        // `c` names nothing bound, so it shares the first level with `a`
        // rather than waiting for `b`.
        assert!(sql.contains("[tuple(x + 1, y + 1)]"), "{sql}");
        assert_eq!(sql.matches("arrayMap(b").count(), 2, "{sql}");
    }

    #[test]
    fn a_name_inside_a_longer_one_is_left_alone() {
        let sql = chain(&values(&[("a", "1")]), "a + ab + a_b");
        assert_eq!(sql, "(1) + ab + a_b");
    }

    /// A chain inside another one has to take a prefix of its own, so the
    /// inner lambda does not shadow the parameter the outer body reads.
    #[test]
    fn a_prefix_names_the_lambda_parameters() {
        let inner = chain_in("pa", &values(&[("a", "x + 1")]), "a + a");
        assert_eq!(inner, "arrayMap(pa0 -> pa0.1 + pa0.1, [tuple(x + 1)])[1]");
        let outer = chain(&values(&[("v", &inner)]), "v + v");
        assert!(
            outer.starts_with("arrayMap(b0 -> b0.1 + b0.1, [tuple(arrayMap(pa0"),
            "{outer}"
        );
    }

    /// Each level maps the one below it, so the second level's tuple and
    /// the body sit in a lambda of their own rather than inside the first
    /// level's.
    #[test]
    fn a_level_sits_beside_the_one_below_it_rather_than_inside_it() {
        let sql = chain(&values(&[("a", "x + 1"), ("b", "a * a")]), "a + b + b");
        assert_eq!(
            sql,
            "arrayMap(b1 -> b1.1 + b1.2 + b1.2, \
             arrayMap(b0 -> tuple(b0.1, b0.1 * b0.1), [tuple(x + 1)]))[1]"
        );
    }

    /// A chain of any length keeps its levels beside each other, so the
    /// third level's tuple is no deeper than the second's.
    #[test]
    fn a_longer_chain_does_not_put_its_levels_deeper() {
        let sql = chain(
            &values(&[("a", "x + 1"), ("b", "a * a"), ("c", "b + 1")]),
            "a + b + c + c",
        );
        assert_eq!(
            sql,
            "arrayMap(b2 -> b2.1 + b2.2 + b2.3 + b2.3, \
             arrayMap(b1 -> tuple(b1.1, b1.2, b1.2 + 1), \
             arrayMap(b0 -> tuple(b0.1, b0.1 * b0.1), [tuple(x + 1)])))[1]"
        );
    }

    #[test]
    fn no_values_is_the_body() {
        assert_eq!(chain(&[], "x"), "x");
    }
}
