//! Binding a value once inside a lambda.
//!
//! A lambda has no `WITH`. Naming a value twice inside one means
//! evaluating it twice and carrying it twice in the query tree, and a
//! chain of such names grows the tree by the product of its branches. A
//! single-element array threads the values through instead: each level
//! binds what the levels after it read into a tuple, and they read it back
//! by position.
//!
//! [`chain`] takes values in dependency order and returns one expression.

/// The lambda parameter each level binds its values into.
fn level_name(at: usize) -> String {
    format!("b{at}")
}

/// `values` bound in order, ending in `body`.
///
/// A value may name the ones before it. Two things keep the chain cheap.
/// A value named once is written into the one place that reads it, so it
/// costs no level. The rest sit at the shallowest level their own
/// dependencies allow, because a level's tuple is expanded into every
/// level above it and depth multiplies.
pub fn chain(values: &[(String, String)], body: &str) -> String {
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
            .map(|index| format!("{}.{index}", level_name(at.wrapping_sub(1))))
            .collect();
        for (_, expr) in level {
            tuple.push(resolve(expr, &carried, at));
        }
        carried.extend(level.iter().map(|(name, _)| name.clone()));
        members.push(tuple);
    }
    let mut sql = resolve(&body, &carried, levels.len());
    for at in (0..levels.len()).rev() {
        sql = format!(
            "arrayMap({} -> {sql}, [tuple({})])[1]",
            level_name(at),
            members[at].join(", ")
        );
    }
    sql
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
fn resolve(expr: &str, carried: &[String], at: usize) -> String {
    let mut out = expr.to_owned();
    for (index, name) in carried.iter().enumerate() {
        out = rename(&out, name, &format!("{}.{}", level_name(at - 1), index + 1));
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

    #[test]
    fn no_values_is_the_body() {
        assert_eq!(chain(&[], "x"), "x");
    }
}
