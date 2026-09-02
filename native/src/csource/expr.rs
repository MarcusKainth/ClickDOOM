//! Evaluating a C integer constant expression.
//!
//! Enough of one for the operators the engine's tables use: the bitwise
//! and shift operators that build a flag word, the arithmetic that scales
//! a `fixed_t`, and unary minus. Everything is evaluated in `i64`.

use super::error::CError;
use super::lex::{Tok, Token};
use super::symbols::Symbols;

/// Evaluates `toks` as one expression. Every token has to be consumed.
pub fn eval(file: &str, toks: &[Tok<'_>], symbols: &Symbols<'_>) -> Result<i64, CError> {
    let mut parser = Parser {
        file,
        toks,
        at: 0,
        symbols,
    };
    let value = parser.binary(0)?;
    match parser.toks.get(parser.at) {
        None => Ok(value),
        Some(tok) => Err(parser.expected("the end of the expression", tok)),
    }
}

/// Binding power of each binary operator, lowest first. An operator not
/// listed here is not part of a constant expression this reads.
const PRECEDENCE: [&[&str]; 6] = [
    &["|"],
    &["^"],
    &["&"],
    &["<<", ">>"],
    &["+", "-"],
    &["*", "/", "%"],
];

struct Parser<'a, 'b> {
    file: &'b str,
    toks: &'b [Tok<'a>],
    at: usize,
    symbols: &'b Symbols<'b>,
}

impl Parser<'_, '_> {
    fn line(&self) -> u32 {
        let at = self.at.min(self.toks.len().saturating_sub(1));
        self.toks.get(at).map_or(0, |t| t.line)
    }

    fn expected(&self, want: &'static str, found: &Tok<'_>) -> CError {
        CError::Expected {
            file: self.file.to_owned(),
            line: found.line,
            want,
            found: found.token.describe(),
        }
    }

    fn end_of_input(&self, want: &'static str) -> CError {
        CError::Expected {
            file: self.file.to_owned(),
            line: self.line(),
            want,
            found: "the end of the expression".to_owned(),
        }
    }

    /// The punctuation at the cursor, if the cursor is on punctuation.
    fn punct(&self) -> Option<&str> {
        match self.toks.get(self.at)?.token {
            Token::Punct(text) => Some(text),
            _ => None,
        }
    }

    fn binary(&mut self, level: usize) -> Result<i64, CError> {
        let Some(operators) = PRECEDENCE.get(level) else {
            return self.unary();
        };
        let mut left = self.binary(level + 1)?;
        while let Some(op) = self.punct().filter(|p| operators.contains(p)) {
            let op = op.to_owned();
            self.at += 1;
            let right = self.binary(level + 1)?;
            left = apply(&op, left, right);
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<i64, CError> {
        match self.punct() {
            Some("-") => {
                self.at += 1;
                Ok(-self.unary()?)
            }
            Some("+") => {
                self.at += 1;
                self.unary()
            }
            Some("~") => {
                self.at += 1;
                Ok(!self.unary()?)
            }
            Some("!") => {
                self.at += 1;
                Ok(i64::from(self.unary()? == 0))
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<i64, CError> {
        let tok = self
            .toks
            .get(self.at)
            .ok_or_else(|| self.end_of_input("a value"))?;
        self.at += 1;
        match &tok.token {
            Token::Int(value) => Ok(*value),
            Token::Ident(name) => self.symbols.get(name).ok_or_else(|| {
                let (file, line, name) = (self.file.to_owned(), tok.line, (*name).to_owned());
                match self.symbols.is_ambiguous(&name) {
                    true => CError::Ambiguous { file, line, name },
                    false => CError::UnknownSymbol { file, line, name },
                }
            }),
            Token::Punct("(") => {
                let value = self.binary(0)?;
                match self.toks.get(self.at) {
                    Some(Tok {
                        token: Token::Punct(")"),
                        ..
                    }) => {
                        self.at += 1;
                        Ok(value)
                    }
                    Some(other) => Err(self.expected("a closing parenthesis", other)),
                    None => Err(self.end_of_input("a closing parenthesis")),
                }
            }
            _ => Err(self.expected("a value", tok)),
        }
    }
}

/// Wrapping arithmetic throughout: a flag word built with `<<` in this
/// source stays inside 32 bits, and wrapping says so rather than panicking
/// in a debug build if one ever does not.
fn apply(op: &str, left: i64, right: i64) -> i64 {
    match op {
        "|" => left | right,
        "^" => left ^ right,
        "&" => left & right,
        "<<" => left.wrapping_shl(right as u32),
        ">>" => left.wrapping_shr(right as u32),
        "+" => left.wrapping_add(right),
        "-" => left.wrapping_sub(right),
        "*" => left.wrapping_mul(right),
        "/" => left.checked_div(right).unwrap_or(0),
        _ => left.checked_rem(right).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csource::lex::lex;

    const HEADER: &str = "#define FRACBITS 16\n\
        #define FRACUNIT (1<<FRACBITS)\n\
        enum { MF_SPECIAL = 1, MF_SOLID = 2, MF_SHOOTABLE = 4 };";

    fn value(text: &str) -> i64 {
        let header = lex("t.h", HEADER).unwrap();
        let mut symbols = Symbols::new();
        symbols.absorb("t.h", &header);
        symbols.resolve().unwrap();
        let toks = lex("t.c", text).unwrap();
        eval("t.c", &toks, &symbols).unwrap()
    }

    #[test]
    fn or_binds_looser_than_shift_and_multiply() {
        assert_eq!(value("1 | 2 << 3"), 17);
        assert_eq!(value("2 * 3 + 4"), 10);
        assert_eq!(value("2 * (3 + 4)"), 14);
    }

    #[test]
    fn resolves_defines_and_enum_constants() {
        assert_eq!(value("16*FRACUNIT"), 1_048_576);
        assert_eq!(value("MF_SOLID|MF_SHOOTABLE"), 6);
    }

    #[test]
    fn reads_unary_minus() {
        assert_eq!(value("-1"), -1);
        assert_eq!(value("- -2"), 2);
    }

    #[test]
    fn an_unknown_name_is_an_error() {
        let symbols = Symbols::new();
        let toks = lex("t.c", "A_Punch").unwrap();
        assert!(matches!(
            eval("t.c", &toks, &symbols),
            Err(CError::UnknownSymbol { .. })
        ));
    }

    #[test]
    fn a_trailing_token_is_an_error() {
        let symbols = Symbols::new();
        let toks = lex("t.c", "1 2").unwrap();
        assert!(matches!(
            eval("t.c", &toks, &symbols),
            Err(CError::Expected { .. })
        ));
    }
}
