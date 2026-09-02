//! A C lexer, only as much of one as the engine's constant tables need.
//!
//! It knows identifiers, integer literals, string literals, punctuation and
//! preprocessor lines. It knows nothing about statements, types or
//! function bodies, and it does not need to: every table this reads is a
//! braced initializer of constants.

use super::error::CError;

/// One token.
#[derive(Clone, Debug, PartialEq)]
pub enum Token<'a> {
    Ident(&'a str),
    Int(i64),
    /// A string literal with its escapes resolved.
    Str(String),
    /// One or two characters of punctuation.
    Punct(&'a str),
    /// A preprocessor line, `#` and continuations removed.
    Directive(String),
}

impl Token<'_> {
    /// How the token reads in an error message.
    pub fn describe(&self) -> String {
        match self {
            Token::Ident(name) => format!("identifier {name}"),
            Token::Int(value) => format!("{value}"),
            Token::Str(text) => format!("{text:?}"),
            Token::Punct(text) => format!("{text:?}"),
            Token::Directive(_) => "a preprocessor line".to_owned(),
        }
    }
}

/// A token and the line it came from.
#[derive(Clone, Debug)]
pub struct Tok<'a> {
    pub token: Token<'a>,
    pub line: u32,
}

/// The two-character operators, longest match first.
const DIGRAPHS: [&str; 8] = ["<<", ">>", "<=", ">=", "==", "!=", "&&", "||"];

/// Lexes `text`, naming it `file` in any error.
pub fn lex<'a>(file: &str, text: &'a str) -> Result<Vec<Tok<'a>>, CError> {
    Lexer {
        file,
        bytes: text.as_bytes(),
        text,
        at: 0,
        line: 1,
    }
    .run()
}

struct Lexer<'a, 'f> {
    file: &'f str,
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
    line: u32,
}

impl<'a> Lexer<'a, '_> {
    fn run(mut self) -> Result<Vec<Tok<'a>>, CError> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia()?;
            let Some(b) = self.peek() else { return Ok(out) };
            let line = self.line;
            let token = match b {
                b'#' => self.directive(),
                b'"' => self.string()?,
                b'\'' => self.char_literal()?,
                b if b.is_ascii_digit() => self.number()?,
                b if b == b'_' || b.is_ascii_alphabetic() => self.ident(),
                _ => self.punct(),
            };
            out.push(Tok { token, line });
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.text[self.at..].starts_with(s)
    }

    /// Advances over `n` bytes, counting the newlines in them.
    fn bump(&mut self, n: usize) -> &'a str {
        let taken = &self.text[self.at..self.at + n];
        self.line += taken.bytes().filter(|b| *b == b'\n').count() as u32;
        self.at += n;
        taken
    }

    fn skip_trivia(&mut self) -> Result<(), CError> {
        loop {
            let rest = &self.text[self.at..];
            let spaces = rest.len() - rest.trim_start().len();
            if spaces > 0 {
                self.bump(spaces);
            } else if rest.starts_with("//") {
                self.bump(rest.find('\n').unwrap_or(rest.len()));
            } else if let Some(body) = rest.strip_prefix("/*") {
                let n = body
                    .find("*/")
                    .ok_or_else(|| self.unterminated("comment"))?;
                self.bump(n + 4);
            } else {
                return Ok(());
            }
        }
    }

    fn unterminated(&self, what: &'static str) -> CError {
        CError::Unterminated {
            file: self.file.to_owned(),
            line: self.line,
            what,
        }
    }

    /// A preprocessor line, joined across backslash continuations.
    fn directive(&mut self) -> Token<'a> {
        self.bump(1);
        let mut body = String::new();
        while self.at < self.bytes.len() {
            let rest = &self.text[self.at..];
            let n = rest.find('\n').unwrap_or(rest.len());
            let line = self.bump(n);
            let Some(head) = line.strip_suffix('\\') else {
                body.push_str(line);
                break;
            };
            body.push_str(head);
            body.push(' ');
            // Step over the newline the continuation escapes.
            self.bump(usize::from(self.at < self.bytes.len()));
        }
        Token::Directive(body.trim().to_owned())
    }

    fn ident(&mut self) -> Token<'a> {
        let rest = &self.text[self.at..];
        let n = rest
            .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
            .unwrap_or(rest.len());
        Token::Ident(self.bump(n))
    }

    fn number(&mut self) -> Result<Token<'a>, CError> {
        let rest = &self.text[self.at..];
        let n = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        let line = self.line;
        let text = self.bump(n);
        let digits = text.trim_end_matches(['u', 'U', 'l', 'L']);
        let value = match digits.strip_prefix("0x").or(digits.strip_prefix("0X")) {
            // Hexadecimal literals in this source reach 0xffffffff, which
            // is a `u32` value written where a signed field reads it back.
            Some(hex) => u64::from_str_radix(hex, 16).map(|v| v as i64),
            None => digits.parse::<i64>(),
        };
        value.map(Token::Int).map_err(|_| CError::BadNumber {
            file: self.file.to_owned(),
            line,
            text: text.to_owned(),
        })
    }

    fn string(&mut self) -> Result<Token<'a>, CError> {
        self.bump(1);
        let mut out = String::new();
        loop {
            let b = self.peek().ok_or_else(|| self.unterminated("string"))?;
            match b {
                b'"' => {
                    self.bump(1);
                    return Ok(Token::Str(out));
                }
                b'\\' => {
                    self.bump(1);
                    let escaped = self.peek().ok_or_else(|| self.unterminated("escape"))?;
                    out.push(unescape(escaped));
                    self.bump(1);
                }
                _ => out.push_str(self.bump(1)),
            }
        }
    }

    /// A character literal, as its numeric value.
    fn char_literal(&mut self) -> Result<Token<'a>, CError> {
        self.bump(1);
        let b = self.peek().ok_or_else(|| self.unterminated("character"))?;
        let value = if b == b'\\' {
            self.bump(1);
            let escaped = self.peek().ok_or_else(|| self.unterminated("escape"))?;
            unescape(escaped) as i64
        } else {
            b as i64
        };
        self.bump(1);
        if self.peek() != Some(b'\'') {
            return Err(self.unterminated("character"));
        }
        self.bump(1);
        Ok(Token::Int(value))
    }

    fn punct(&mut self) -> Token<'a> {
        let width = if DIGRAPHS.iter().any(|d| self.starts_with(d)) {
            2
        } else {
            1
        };
        Token::Punct(self.bump(width))
    }
}

fn unescape(b: u8) -> char {
    match b {
        b'0' => '\0',
        b'n' => '\n',
        b't' => '\t',
        b'r' => '\r',
        other => other as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<Token<'_>> {
        lex("t.c", text)
            .unwrap()
            .into_iter()
            .map(|t| t.token)
            .collect()
    }

    #[test]
    fn splits_an_initializer_into_tokens() {
        assert_eq!(
            kinds("{SPR_TROO,0,-1,{NULL},S_NULL}"),
            [
                Token::Punct("{"),
                Token::Ident("SPR_TROO"),
                Token::Punct(","),
                Token::Int(0),
                Token::Punct(","),
                Token::Punct("-"),
                Token::Int(1),
                Token::Punct(","),
                Token::Punct("{"),
                Token::Ident("NULL"),
                Token::Punct("}"),
                Token::Punct(","),
                Token::Ident("S_NULL"),
                Token::Punct("}"),
            ]
        );
    }

    #[test]
    fn drops_both_comment_forms() {
        assert_eq!(
            kinds("a // b\n/* c\n d */ e"),
            [Token::Ident("a"), Token::Ident("e")]
        );
    }

    #[test]
    fn reads_hex_and_suffixed_literals() {
        assert_eq!(
            kinds("0x2000000 16u 0xffffffff"),
            [
                Token::Int(0x200_0000),
                Token::Int(16),
                Token::Int(0xffff_ffff),
            ]
        );
    }

    #[test]
    fn joins_a_continued_directive() {
        assert_eq!(
            kinds("#define A 1 \\\n    + 2\nB"),
            [
                Token::Directive("define A 1      + 2".to_owned()),
                Token::Ident("B"),
            ]
        );
    }

    #[test]
    fn resolves_string_escapes() {
        assert_eq!(
            kinds(r#""\0" "SW1BRCOM""#),
            [
                Token::Str("\0".to_owned()),
                Token::Str("SW1BRCOM".to_owned()),
            ]
        );
    }

    #[test]
    fn counts_lines_across_trivia() {
        let toks = lex("t.c", "a\n\n/* two\nlines */ b").unwrap();
        assert_eq!((toks[0].line, toks[1].line), (1, 4));
    }

    #[test]
    fn an_unterminated_comment_is_an_error() {
        assert!(lex("t.c", "a /* b").is_err());
        assert!(lex("t.c", "\"abc").is_err());
    }
}
