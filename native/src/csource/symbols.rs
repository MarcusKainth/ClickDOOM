//! The integer constants the engine's table initializers name.
//!
//! Two sources: enumerator lists, and object-like `#define`s whose body is
//! an integer constant expression. A definition whose body this cannot
//! evaluate is dropped rather than guessed at, so a name that survives
//! carries the value the compiler would give it.

use std::collections::{BTreeMap, BTreeSet};

use super::error::CError;
use super::expr;
use super::lex::{Tok, Token, lex};

/// A definition waiting on the names it mentions.
enum Pending<'a> {
    /// A `#define` body, lexed when it is evaluated.
    Define { file: String, body: String },
    /// An enumerator's explicit value.
    Value { file: String, toks: Vec<Tok<'a>> },
    /// An enumerator with no `=`: one past the enumerator before it.
    After(String),
    /// The first enumerator of a list, which is zero.
    Zero,
}

/// Name to value, plus the definitions not yet evaluated.
#[derive(Default)]
pub struct Symbols<'a> {
    values: BTreeMap<String, i64>,
    ambiguous: BTreeSet<String>,
    pending: Vec<(String, Pending<'a>)>,
}

impl<'a> Symbols<'a> {
    /// A table holding only `NULL`, which the C library defines and this
    /// reader never sees the definition of.
    pub fn new() -> Symbols<'a> {
        Symbols {
            values: BTreeMap::from([("NULL".to_owned(), 0)]),
            ambiguous: BTreeSet::new(),
            pending: Vec::new(),
        }
    }

    pub fn get(&self, name: &str) -> Option<i64> {
        self.values.get(name).copied()
    }

    /// True for a name this reader saw two different definitions of. It
    /// has no preprocessor, so it sees both arms of an `#ifdef` and
    /// cannot say which one the compiler takes.
    pub fn is_ambiguous(&self, name: &str) -> bool {
        self.ambiguous.contains(name)
    }

    /// Collects every definition in `toks` without evaluating any of them.
    /// [`resolve`](Symbols::resolve) is what turns them into values, and
    /// until it runs a name defined in a later file is still unknown.
    pub fn absorb(&mut self, file: &str, toks: &'a [Tok<'a>]) {
        let mut at = 0;
        while at < toks.len() {
            match &toks[at].token {
                Token::Directive(body) => {
                    self.absorb_define(file, body);
                    at += 1;
                }
                Token::Ident("enum") => at = self.absorb_enum(file, toks, at + 1),
                _ => at += 1,
            }
        }
    }

    /// `#define NAME <expression>`. A macro taking parameters, or one with
    /// an empty body, is not a constant and is skipped.
    fn absorb_define(&mut self, file: &str, body: &str) {
        let Some(rest) = body.strip_prefix("define ") else {
            return;
        };
        let rest = rest.trim_start();
        let name_len = rest
            .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
            .unwrap_or(rest.len());
        let (name, tail) = rest.split_at(name_len);
        if name.is_empty() || tail.starts_with('(') || tail.trim().is_empty() {
            return;
        }
        self.pending.push((
            name.to_owned(),
            Pending::Define {
                file: file.to_owned(),
                body: tail.trim().to_owned(),
            },
        ));
    }

    /// An enumerator list, starting at the token after `enum`. Returns the
    /// index just past the closing brace.
    fn absorb_enum(&mut self, file: &str, toks: &'a [Tok<'a>], mut at: usize) -> usize {
        // An optional tag sits between `enum` and the brace.
        if matches!(toks.get(at).map(|t| &t.token), Some(Token::Ident(_))) {
            at += 1;
        }
        if !matches!(toks.get(at).map(|t| &t.token), Some(Token::Punct("{"))) {
            return at;
        }
        at += 1;
        let mut previous: Option<String> = None;
        while let Some(tok) = toks.get(at) {
            let name = match &tok.token {
                Token::Ident(name) => (*name).to_owned(),
                _ => return at + 1,
            };
            at += 1;
            let definition = if matches!(toks.get(at).map(|t| &t.token), Some(Token::Punct("="))) {
                at += 1;
                let start = at;
                at = skip_to_separator(toks, at);
                Pending::Value {
                    file: file.to_owned(),
                    toks: toks[start..at].to_vec(),
                }
            } else {
                match &previous {
                    Some(before) => Pending::After(before.clone()),
                    None => Pending::Zero,
                }
            };
            self.pending.push((name.clone(), definition));
            previous = Some(name);
            if matches!(toks.get(at).map(|t| &t.token), Some(Token::Punct(","))) {
                at += 1;
            }
        }
        at
    }

    /// Evaluates every collected definition, repeating while progress is
    /// made. A definition that never resolves is dropped: the headers hold
    /// macros that are not integers, and no table names one.
    ///
    /// A name defined twice with two different values becomes ambiguous
    /// rather than taking either value, and naming one in a table is what
    /// turns that into an error.
    pub fn resolve(&mut self) -> Result<(), CError> {
        while !self.pending.is_empty() {
            let mut resolved = 0;
            let mut deferred = Vec::new();
            for (name, definition) in std::mem::take(&mut self.pending) {
                match self.evaluate(&definition)? {
                    Some(value) => {
                        self.define(name, value);
                        resolved += 1;
                    }
                    None => deferred.push((name, definition)),
                }
            }
            if resolved == 0 {
                return Ok(());
            }
            self.pending = deferred;
        }
        Ok(())
    }

    /// The definition's value, or `None` while a name it mentions is
    /// still unknown.
    fn evaluate(&self, definition: &Pending<'a>) -> Result<Option<i64>, CError> {
        Ok(match definition {
            Pending::Zero => Some(0),
            Pending::After(before) => self.get(before).map(|value| value + 1),
            Pending::Value { file, toks } => expr::eval(file, toks, self).ok(),
            Pending::Define { file, body } => expr::eval(file, &lex(file, body)?, self).ok(),
        })
    }

    fn define(&mut self, name: String, value: i64) {
        if self.ambiguous.contains(&name) {
            return;
        }
        if let Some(first) = self.values.insert(name.clone(), value)
            && first != value
        {
            self.values.remove(&name);
            self.ambiguous.insert(name);
        }
    }
}

/// Walks to the next `,` or `}` outside brackets or parentheses.
pub fn skip_to_separator(toks: &[Tok<'_>], mut at: usize) -> usize {
    let mut depth = 0i32;
    while let Some(tok) = toks.get(at) {
        match tok.token {
            Token::Punct("(") | Token::Punct("[") | Token::Punct("{") => depth += 1,
            Token::Punct(")") | Token::Punct("]") => depth -= 1,
            Token::Punct("}") if depth == 0 => return at,
            Token::Punct("}") => depth -= 1,
            Token::Punct(",") if depth == 0 => return at,
            _ => {}
        }
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_of(text: &str, name: &str) -> Option<i64> {
        let toks = lex("t.h", text).unwrap();
        let mut symbols = Symbols::new();
        symbols.absorb("t.h", &toks);
        symbols.resolve().unwrap();
        symbols.get(name)
    }

    #[test]
    fn numbers_an_enum_from_zero() {
        let text = "typedef enum { am_clip, am_shell, am_cell } ammotype_t;";
        assert_eq!(value_of(text, "am_clip"), Some(0));
        assert_eq!(value_of(text, "am_cell"), Some(2));
    }

    #[test]
    fn an_explicit_value_moves_the_ones_after_it() {
        let text = "enum { a, b = 8, c, d = 0x400, e };";
        assert_eq!(value_of(text, "c"), Some(9));
        assert_eq!(value_of(text, "e"), Some(0x401));
    }

    #[test]
    fn a_define_resolves_through_a_later_define() {
        let text = "#define FRACUNIT (1<<FRACBITS)\n#define FRACBITS 16\n";
        assert_eq!(value_of(text, "FRACUNIT"), Some(65536));
    }

    #[test]
    fn a_macro_with_parameters_is_not_a_constant() {
        let text = "#define FixedMul(a,b) ((a)*(b))\n#define MAXPLAYERS 4\n";
        assert_eq!(value_of(text, "FixedMul"), None);
        assert_eq!(value_of(text, "MAXPLAYERS"), Some(4));
    }

    #[test]
    fn a_body_that_is_not_an_integer_is_dropped() {
        let text = "#define PACKAGE_NAME \"doom\"\n#define TICRATE 35\n";
        assert_eq!(value_of(text, "PACKAGE_NAME"), None);
        assert_eq!(value_of(text, "TICRATE"), Some(35));
    }

    /// `doomtype.h` defines `DIR_SEPARATOR` twice, once per arm of an
    /// `#ifdef`. Neither value is this reader's to pick.
    #[test]
    fn two_values_for_one_name_leave_it_ambiguous() {
        let toks = lex("t.h", "#define K 1\n#define K 2\n#define J 3\n").unwrap();
        let mut symbols = Symbols::new();
        symbols.absorb("t.h", &toks);
        symbols.resolve().unwrap();
        assert_eq!(symbols.get("K"), None);
        assert!(symbols.is_ambiguous("K"));
        assert_eq!(symbols.get("J"), Some(3));
        assert!(!symbols.is_ambiguous("J"));
    }
}
