//! Each comparison that is an operand of `AND` or `OR`, wrapped in
//! `identity()`.
//!
//! `identity` returns its argument as it is, so a wrapped statement computes
//! the same values, and short-circuiting reaches through it to the
//! comparison's own arguments. The rewrite reads the text as tokens. It
//! follows brackets, lambdas, lists, subqueries, strings, quoted names and
//! comments. A `BETWEEN` and its own `AND` stay one operand, left as written.

/// What a token is, as far as finding a chain's operands goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A name or a keyword.
    Word,
    /// A number, a string or a quoted name.
    Literal,
    Open,
    Close,
    Comma,
    Arrow,
    /// `=`, `==`, `!=`, `<>`, `<`, `<=`, `>`, `>=`.
    Compare,
    /// The ternary's `?` or `:`, which end an operand.
    Ternary,
    Other,
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
    start: usize,
    end: usize,
}

/// `sql` as tokens, without its whitespace and comments.
fn tokens(sql: &str) -> Vec<Token<'_>> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        let c = bytes[at];
        let rest = &sql[at..];
        let kind = if c.is_ascii_whitespace() {
            at += 1;
            continue;
        } else if rest.starts_with("--") {
            at += rest.find('\n').unwrap_or(rest.len());
            continue;
        } else if rest.starts_with("/*") {
            at += rest.find("*/").map_or(rest.len(), |close| close + 2);
            continue;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            at += word_length(rest);
            Kind::Word
        } else if c.is_ascii_digit() {
            at += word_length(rest);
            Kind::Literal
        } else if matches!(c, b'\'' | b'"' | b'`') {
            at += quoted_length(rest);
            Kind::Literal
        } else {
            let (kind, length) = match (c, bytes.get(at + 1).copied()) {
                (b'(' | b'[', _) => (Kind::Open, 1),
                (b')' | b']', _) => (Kind::Close, 1),
                (b',', _) => (Kind::Comma, 1),
                (b'-', Some(b'>')) => (Kind::Arrow, 2),
                (b':', Some(b':')) => (Kind::Other, 2),
                (b'?' | b':', _) => (Kind::Ternary, 1),
                (b'<', Some(b'=' | b'>')) | (b'>' | b'=' | b'!', Some(b'=')) => (Kind::Compare, 2),
                (b'<' | b'>' | b'=', _) => (Kind::Compare, 1),
                _ => (Kind::Other, rest.chars().next().map_or(1, char::len_utf8)),
            };
            at += length;
            kind
        };
        out.push(Token {
            kind,
            text: &sql[start..at],
            start,
            end: at,
        });
    }
    out
}

/// How many bytes of `text`'s head are letters, digits or underscores.
fn word_length(text: &str) -> usize {
    text.bytes()
        .position(|b| !(b.is_ascii_alphanumeric() || b == b'_'))
        .unwrap_or(text.len())
}

/// How many bytes the quoted token at `text`'s head takes, both quotes
/// included. A backslash escapes the byte after it.
fn quoted_length(text: &str) -> usize {
    let bytes = text.as_bytes();
    let quote = bytes[0];
    let mut at = 1;
    while at < bytes.len() && bytes[at] != quote {
        at += if bytes[at] == b'\\' { 2 } else { 1 };
    }
    (at + 1).min(bytes.len())
}

/// Whether `word` is one of `known`, in any case.
fn is(word: &str, known: &[&str]) -> bool {
    known.iter().any(|known| known.eq_ignore_ascii_case(word))
}

/// Keywords that end one expression and start the next.
const CLAUSES: [&str; 20] = [
    "SELECT", "FROM", "WHERE", "PREWHERE", "HAVING", "AS", "ON", "USING", "JOIN", "BY", "LIMIT",
    "SETTINGS", "UNION", "INSERT", "INTO", "WITH", "CASE", "WHEN", "THEN", "ELSE",
];

/// Keywords that sit inside an expression without naming anything.
const OPERATORS: [&str; 10] = [
    "AND", "OR", "NOT", "IN", "GLOBAL", "BETWEEN", "LIKE", "ILIKE", "IS", "DISTINCT",
];

/// The functions a comparison operator stands for.
const COMPARISONS: [&str; 6] = [
    "equals",
    "notEquals",
    "less",
    "lessOrEquals",
    "greater",
    "greaterOrEquals",
];

/// Whether `token` is where an operand can end, so an `AND` or `OR` after
/// it joins two operands rather than naming a function.
fn ends_operand(token: &Token<'_>) -> bool {
    match token.kind {
        Kind::Literal | Kind::Close => true,
        Kind::Word => !is(token.text, &CLAUSES) && !is(token.text, &OPERATORS),
        _ => false,
    }
}

/// One operand of a run: where it starts and ends, and whether it is a
/// comparison.
#[derive(Default)]
struct Operand {
    start: Option<usize>,
    end: usize,
    /// The tokens and brackets at the operand's own depth.
    items: usize,
    /// A comparison operator sits at the operand's own depth.
    compares: bool,
    /// The operand is one bracketed comparison or one comparison function.
    one_comparison: bool,
}

impl Operand {
    fn take(&mut self, token: &Token<'_>) {
        self.start.get_or_insert(token.start);
        self.end = token.end;
        self.items += 1;
        self.one_comparison = false;
        self.compares |= token.kind == Kind::Compare;
    }

    fn is_comparison(&self) -> bool {
        self.compares || self.one_comparison
    }
}

/// The operands between two breaks at one depth: a comma, a lambda's
/// arrow, a clause keyword or the ternary's marks.
#[derive(Default)]
struct Run {
    operands: Vec<Operand>,
    /// An `AND` or an `OR` joins the operands.
    chain: bool,
    /// A `BETWEEN` waits for its own `AND`.
    between: bool,
}

impl Run {
    fn chain() -> Run {
        Run {
            chain: true,
            ..Run::default()
        }
    }

    fn operand(&mut self) -> &mut Operand {
        if self.operands.is_empty() {
            self.operands.push(Operand::default());
        }
        self.operands.last_mut().expect("pushed above")
    }

    /// Whether the run is one comparison and nothing else.
    fn is_comparison(&self) -> bool {
        !self.chain && self.operands.len() == 1 && self.operands[0].is_comparison()
    }

    /// The spans of the run's comparison operands, if the run is a chain.
    fn finish(self, spans: &mut Vec<(usize, usize)>) {
        if !self.chain {
            return;
        }
        for operand in self.operands {
            if let (Some(start), true) = (operand.start, operand.is_comparison()) {
                spans.push((start, operand.end));
            }
        }
    }
}

/// What opened a bracket.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bracket {
    /// The statement itself, which no bracket opened.
    Top,
    /// A bracket around an expression, a tuple or a subquery.
    Group,
    /// A call to a function. `and(...)` and `or(...)` chain their
    /// arguments, and a call to a comparison function is a comparison.
    Call { chain: bool, comparison: bool },
    /// An array literal or a subscript.
    Square,
}

struct Level {
    bracket: Bracket,
    run: Run,
    /// A break split the bracket, so it holds a list or a subquery.
    split: bool,
}

impl Level {
    fn new(bracket: Bracket) -> Level {
        let run = match bracket {
            Bracket::Call { chain: true, .. } => Run::chain(),
            _ => Run::default(),
        };
        Level {
            bracket,
            run,
            split: false,
        }
    }

    /// Ends the current run at a break.
    fn split(&mut self, spans: &mut Vec<(usize, usize)>) {
        let run = std::mem::take(&mut self.run);
        run.finish(spans);
        self.split = true;
    }

    /// Whether the whole bracket, once closed, is one comparison.
    fn is_comparison(&self) -> bool {
        match self.bracket {
            Bracket::Group => !self.split && self.run.is_comparison(),
            Bracket::Call { comparison, .. } => comparison,
            Bracket::Top | Bracket::Square => false,
        }
    }
}

/// The byte span of each comparison that is an operand of `AND` or `OR`
/// in `sql`, including one that is an argument of `and()` or `or()`.
pub(super) fn chain_comparisons(sql: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut levels = vec![Level::new(Bracket::Top)];
    let mut previous: Option<Token<'_>> = None;
    // The token before names the function a `(` after it calls.
    let mut names = false;
    for token in tokens(sql) {
        let infix = previous.as_ref().is_some_and(ends_operand);
        let depth = levels.len();
        let level = levels.last_mut().expect("the top level stays");
        match token.kind {
            Kind::Open => {
                level.run.operand().take(&token);
                let bracket = match previous {
                    _ if token.text == "[" => Bracket::Square,
                    Some(name) if names => Bracket::Call {
                        chain: is(name.text, &["and", "or"]),
                        comparison: COMPARISONS.contains(&name.text),
                    },
                    _ => Bracket::Group,
                };
                levels.push(Level::new(bracket));
            }
            Kind::Close if depth > 1 => {
                let inner = levels.pop().expect("more than the top level");
                let comparison = inner.is_comparison();
                // What the operand holds when the bracket is all of it.
                let items = match inner.bracket {
                    Bracket::Call { .. } => 2,
                    _ => 1,
                };
                inner.run.finish(&mut spans);
                let operand = levels
                    .last_mut()
                    .expect("the top level stays")
                    .run
                    .operand();
                operand.end = token.end;
                operand.one_comparison = comparison && operand.items == items;
            }
            Kind::Comma if matches!(level.bracket, Bracket::Call { chain: true, .. }) => {
                level.run.operands.push(Operand::default());
            }
            Kind::Comma | Kind::Arrow | Kind::Ternary => level.split(&mut spans),
            Kind::Word if is(token.text, &CLAUSES) => level.split(&mut spans),
            Kind::Word if is(token.text, &["BETWEEN"]) => {
                level.run.between = true;
                level.run.operand().take(&token);
            }
            Kind::Word if is(token.text, &["AND"]) && level.run.between => {
                level.run.between = false;
                level.run.operand().take(&token);
            }
            Kind::Word if infix && is(token.text, &["AND", "OR"]) => {
                level.run.chain = true;
                level.run.operands.push(Operand::default());
            }
            _ => level.run.operand().take(&token),
        }
        names = match token.kind {
            Kind::Close => true,
            Kind::Word if is(token.text, &["AND", "OR"]) => !infix,
            Kind::Word => !is(token.text, &CLAUSES) && !is(token.text, &OPERATORS),
            _ => false,
        };
        previous = Some(token);
    }
    for level in levels.into_iter().rev() {
        level.run.finish(&mut spans);
    }
    spans
}

/// `sql` with each comparison that is an operand of `AND` or `OR` wrapped
/// in `identity()`.
pub(super) fn wrap_chain_comparisons(sql: &str) -> String {
    const OPEN: &str = "identity(";
    let spans = chain_comparisons(sql);
    // Each span's two ends, in text order. At one byte a close, `false`,
    // sorts ahead of an open.
    let mut marks: Vec<(usize, bool)> = spans
        .iter()
        .flat_map(|&(start, end)| [(start, true), (end, false)])
        .collect();
    marks.sort_unstable();
    let mut out = String::with_capacity(sql.len() + spans.len() * (OPEN.len() + 1));
    let mut copied = 0;
    for (at, open) in marks {
        out.push_str(&sql[copied..at]);
        out.push_str(if open { OPEN } else { ")" });
        copied = at;
    }
    out.push_str(&sql[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::{chain_comparisons, wrap_chain_comparisons as wrap};

    #[test]
    fn wraps_each_comparison_of_a_chain() {
        assert_eq!(wrap("a = 1 AND b"), "identity(a = 1) AND b");
        assert_eq!(
            wrap("a != 1 OR b <> 2 OR c < 3 OR d >= 4 OR e == 5"),
            "identity(a != 1) OR identity(b <> 2) OR identity(c < 3) OR identity(d >= 4) \
             OR identity(e == 5)"
        );
        assert_eq!(
            wrap("f(a) = 1 AND (b AND c = 2)"),
            "identity(f(a) = 1) AND (b AND identity(c = 2))"
        );
        assert_eq!(
            wrap("a = 1 OR b = 2 AND c = 3"),
            "identity(a = 1) OR identity(b = 2) AND identity(c = 3)"
        );
        assert_eq!(wrap("NOT a = 1 AND b"), "identity(NOT a = 1) AND b");
        assert_eq!(wrap("a.1 = 1 and b"), "identity(a.1 = 1) and b");
    }

    #[test]
    fn leaves_what_is_not_a_chain() {
        for sql in [
            "a = 1",
            "f(a = 1, b < 2)",
            "(a = 1) AS b, c",
            "if(a = 1, b, c)",
            "WHERE tic > 0",
            "a AND b",
            "NOT (a = 1) AND b",
            "a IN (1, 2) AND b",
            "a + (b = 1) AND c",
        ] {
            assert_eq!(wrap(sql), sql);
        }
    }

    #[test]
    fn wraps_a_bracketed_comparison_as_a_whole() {
        assert_eq!(
            wrap("if(x.1 = 1 OR (y != -1), z, 0)"),
            "if(identity(x.1 = 1) OR identity((y != -1)), z, 0)"
        );
        assert_eq!(wrap("((a = 1)) AND b"), "identity(((a = 1))) AND b");
        assert_eq!(wrap("(a, b = 1) AND c"), "(a, b = 1) AND c");
        assert_eq!(wrap("[a = 1] AND c"), "[a = 1] AND c");
    }

    #[test]
    fn a_chain_inside_a_comparison_is_wrapped_on_both_levels() {
        assert_eq!(
            wrap("(a = 1 AND b) = c OR d"),
            "identity((identity(a = 1) AND b) = c) OR d"
        );
    }

    #[test]
    fn between_keeps_its_own_and() {
        assert_eq!(wrap("x BETWEEN 1 AND 2"), "x BETWEEN 1 AND 2");
        assert_eq!(
            wrap("x BETWEEN 1 AND 2 AND y = 3"),
            "x BETWEEN 1 AND 2 AND identity(y = 3)"
        );
        assert_eq!(
            wrap("a = 1 AND x NOT BETWEEN 1 AND 2 OR b = 2"),
            "identity(a = 1) AND x NOT BETWEEN 1 AND 2 OR identity(b = 2)"
        );
    }

    #[test]
    fn an_in_list_is_one_operand() {
        assert_eq!(
            wrap("a IN (1, 2) AND b = 1"),
            "a IN (1, 2) AND identity(b = 1)"
        );
        assert_eq!(
            wrap("a IN (x = 1 AND y, 2) OR b"),
            "a IN (identity(x = 1) AND y, 2) OR b"
        );
    }

    #[test]
    fn a_subquery_ends_the_operands_around_it() {
        assert_eq!(
            wrap("(SELECT a > 0 AND b FROM t WHERE c = 1 AND d) = 1 AND e"),
            "identity((SELECT identity(a > 0) AND b FROM t WHERE identity(c = 1) AND d) = 1) AND e"
        );
        assert_eq!(
            wrap("x = (SELECT max(tic) FROM t) OR y"),
            "identity(x = (SELECT max(tic) FROM t)) OR y"
        );
    }

    #[test]
    fn a_string_or_a_comment_hides_its_operators() {
        assert_eq!(
            wrap("SELECT a > 0 AND 'x=1 AND (' = s FROM t"),
            "SELECT identity(a > 0) AND identity('x=1 AND (' = s) FROM t"
        );
        assert_eq!(
            wrap("s = 'it\\'s < ok' OR \"a=b\" = 1"),
            "identity(s = 'it\\'s < ok') OR identity(\"a=b\" = 1)"
        );
        assert_eq!(
            wrap("a -- b = 1 AND c\nAND d = 1"),
            "a -- b = 1 AND c\nAND identity(d = 1)"
        );
        assert_eq!(wrap("a /* ( */ = 1 AND b"), "identity(a /* ( */ = 1) AND b");
    }

    #[test]
    fn a_lambda_s_body_is_a_run_of_its_own() {
        assert_eq!(
            wrap("arrayMap(k -> k >= 2 AND g(k) <= 3, xs)"),
            "arrayMap(k -> identity(k >= 2) AND identity(g(k) <= 3), xs)"
        );
        assert_eq!(
            wrap("arrayExists((t, l) -> t != 0 AND l = 1, a, b) AND c"),
            "arrayExists((t, l) -> identity(t != 0) AND identity(l = 1), a, b) AND c"
        );
    }

    #[test]
    fn a_call_to_and_or_or_chains_its_arguments() {
        assert_eq!(
            wrap("and(a = 1, b, equals(c, 2))"),
            "and(identity(a = 1), b, identity(equals(c, 2)))"
        );
        assert_eq!(wrap("x AND (a = 1)"), "x AND identity((a = 1))");
        assert_eq!(wrap("f(or(a < 1, b))"), "f(or(identity(a < 1), b))");
        assert_eq!(wrap("less(a, 1) OR b"), "identity(less(a, 1)) OR b");
    }

    #[test]
    fn the_ternary_ends_an_operand() {
        assert_eq!(
            wrap("a AND b = 1 ? x : y = 2 OR z"),
            "a AND identity(b = 1) ? x : identity(y = 2) OR z"
        );
    }

    #[test]
    fn a_wrapped_statement_holds_no_chain_comparison() {
        let sql = "SELECT a = 1 AND (b > 2 OR c) FROM t WHERE x = 1 AND y IN (1, 2)";
        assert_eq!(chain_comparisons(sql).len(), 3);
        assert!(chain_comparisons(&wrap(sql)).is_empty());
    }
}
