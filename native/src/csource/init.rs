//! Braced initializers and struct shapes.
//!
//! An initializer is read as a tree of raw token runs. Nothing is
//! evaluated while reading, because the leaves are not all integers: a
//! `state_t` names an action function, and an `animdef_t` names a flat.
//! Each caller says what it expects a leaf to be.

use super::error::CError;
use super::expr;
use super::lex::{Tok, Token};
use super::symbols::Symbols;

/// What a leaf is read against: the file it came from, for error
/// messages, and the constants an integer leaf may name.
pub struct Ctx<'s, 'a> {
    pub file: &'s str,
    pub symbols: &'s Symbols<'a>,
}

/// One initializer: a braced list, or a run of tokens between separators.
#[derive(Clone, Debug)]
pub enum Node<'a> {
    Leaf { line: u32, toks: &'a [Tok<'a>] },
    List { line: u32, items: Vec<Node<'a>> },
}

/// An array declaration and its initializer.
pub struct Array<'a> {
    /// The declared bounds, one per `[...]`. `None` for a `[]` with no
    /// bound, which the initializer's own length then fixes.
    pub bounds: Vec<Option<i64>>,
    pub root: Node<'a>,
}

impl<'a> Node<'a> {
    pub fn line(&self) -> u32 {
        match self {
            Node::Leaf { line, .. } | Node::List { line, .. } => *line,
        }
    }

    fn expected(&self, ctx: &Ctx<'_, 'a>, want: &'static str) -> CError {
        let found = match self {
            Node::List { .. } => "a braced list".to_owned(),
            Node::Leaf { toks, .. } => match toks.first() {
                Some(tok) => tok.token.describe(),
                None => "an empty initializer".to_owned(),
            },
        };
        CError::Expected {
            file: ctx.file.to_owned(),
            line: self.line(),
            want,
            found,
        }
    }

    /// The entries of a braced list.
    pub fn list(&self, ctx: &Ctx<'_, 'a>) -> Result<&[Node<'a>], CError> {
        match self {
            Node::List { items, .. } => Ok(items),
            _ => Err(self.expected(ctx, "a braced list")),
        }
    }

    /// A leaf as an integer constant expression. A one-entry braced list
    /// holding one too: C writes a union's initializer that way.
    pub fn int(&self, ctx: &Ctx<'_, 'a>) -> Result<i64, CError> {
        match self {
            Node::Leaf { toks, .. } => expr::eval(ctx.file, toks, ctx.symbols),
            Node::List { items, .. } if items.len() == 1 => items[0].int(ctx),
            _ => Err(self.expected(ctx, "an integer")),
        }
    }

    /// A leaf naming one identifier, kept as its name. A one-entry braced
    /// list holding one too, which is how `state_t` writes its action.
    pub fn name(&self, ctx: &Ctx<'_, 'a>) -> Result<&'a str, CError> {
        match self {
            Node::Leaf {
                toks:
                    [
                        Tok {
                            token: Token::Ident(name),
                            ..
                        },
                    ],
                ..
            } => Ok(name),
            Node::List { items, .. } if items.len() == 1 => items[0].name(ctx),
            _ => Err(self.expected(ctx, "one identifier")),
        }
    }

    /// A leaf that is a string literal, as the C string it initializes: up
    /// to the first NUL, since that is where the array's own content ends.
    pub fn text(&self, ctx: &Ctx<'_, 'a>) -> Result<&'a str, CError> {
        match self {
            Node::Leaf {
                toks:
                    [
                        Tok {
                            token: Token::Str(text),
                            ..
                        },
                    ],
                ..
            } => Ok(text.split('\0').next().unwrap_or_default()),
            _ => Err(self.expected(ctx, "a string literal")),
        }
    }

    /// The entries of a braced list, zero-filled up to `arity` the way C
    /// fills the tail of a partial initializer.
    pub fn ints(&self, ctx: &Ctx<'_, 'a>, arity: usize) -> Result<Vec<i64>, CError> {
        let items = self.list(ctx)?;
        let mut out = Vec::with_capacity(arity);
        for item in items.iter().take(arity) {
            out.push(item.int(ctx)?);
        }
        out.resize(arity, 0);
        Ok(out)
    }
}

/// Finds `name`'s array declaration and reads its initializer.
///
/// The declaration is an identifier followed by its bounds, `=`, and a
/// brace. A mention of the array anywhere else, in a comment or as a
/// subscript, does not match.
pub fn find_array<'a>(
    ctx: &Ctx<'_, 'a>,
    toks: &'a [Tok<'a>],
    name: &str,
) -> Result<Array<'a>, CError> {
    for at in 0..toks.len() {
        if toks[at].token != Token::Ident(name) {
            continue;
        }
        let Some((bounds, after)) = read_bounds(ctx, toks, at + 1)? else {
            continue;
        };
        if toks.get(after).map(|t| &t.token) != Some(&Token::Punct("=")) {
            continue;
        }
        let (root, _) = read_node(toks, after + 1);
        if matches!(root, Node::List { .. }) {
            return Ok(Array { bounds, root });
        }
    }
    Err(CError::NoArray {
        file: ctx.file.to_owned(),
        name: name.to_owned(),
    })
}

/// The `[...]` groups after a declared name, and the index past them.
/// `None` when the name is not followed by one.
type Bounds = Option<(Vec<Option<i64>>, usize)>;

fn read_bounds<'a>(
    ctx: &Ctx<'_, 'a>,
    toks: &'a [Tok<'a>],
    mut at: usize,
) -> Result<Bounds, CError> {
    let mut bounds = Vec::new();
    while toks.get(at).map(|t| &t.token) == Some(&Token::Punct("[")) {
        let start = at + 1;
        at = start;
        while toks
            .get(at)
            .map(|t| &t.token)
            .is_some_and(|t| *t != Token::Punct("]"))
        {
            at += 1;
        }
        bounds.push(match start == at {
            true => None,
            false => Some(expr::eval(ctx.file, &toks[start..at], ctx.symbols)?),
        });
        at += 1;
    }
    Ok((!bounds.is_empty()).then_some((bounds, at)))
}

/// Reads one initializer starting at `at`, and returns the index past it.
fn read_node<'a>(toks: &'a [Tok<'a>], at: usize) -> (Node<'a>, usize) {
    let line = toks.get(at).map_or(0, |t| t.line);
    if toks.get(at).map(|t| &t.token) != Some(&Token::Punct("{")) {
        let end = leaf_end(toks, at);
        return (
            Node::Leaf {
                line,
                toks: &toks[at..end],
            },
            end,
        );
    }
    let mut items = Vec::new();
    let mut at = at + 1;
    loop {
        match toks.get(at).map(|t| &t.token) {
            None => return (Node::List { line, items }, at),
            Some(Token::Punct("}")) => return (Node::List { line, items }, at + 1),
            Some(Token::Punct(",")) => at += 1,
            _ => {
                let (node, next) = read_node(toks, at);
                items.push(node);
                at = next;
            }
        }
    }
}

/// The end of a leaf: the next `,` or `}` outside parentheses.
fn leaf_end(toks: &[Tok<'_>], mut at: usize) -> usize {
    let mut depth = 0i32;
    while let Some(tok) = toks.get(at) {
        match tok.token {
            Token::Punct("(") | Token::Punct("[") => depth += 1,
            Token::Punct(")") | Token::Punct("]") => depth -= 1,
            Token::Punct(",") | Token::Punct("}") if depth == 0 => return at,
            _ => {}
        }
        at += 1;
    }
    at
}

/// The field names of `typedef struct { ... } name;`, in declaration
/// order.
pub fn struct_fields<'a>(
    ctx: &Ctx<'_, 'a>,
    toks: &'a [Tok<'a>],
    name: &str,
) -> Result<Vec<&'a str>, CError> {
    for at in 0..toks.len() {
        if toks[at].token != Token::Ident("struct") {
            continue;
        }
        // An optional tag sits between `struct` and the brace.
        let open = at
            + 1
            + usize::from(matches!(
                toks.get(at + 1).map(|t| &t.token),
                Some(Token::Ident(_))
            ));
        if toks.get(open).map(|t| &t.token) != Some(&Token::Punct("{")) {
            continue;
        }
        let Some(close) = matching_brace(toks, open) else {
            continue;
        };
        if toks.get(close + 1).map(|t| &t.token) != Some(&Token::Ident(name)) {
            continue;
        }
        return Ok(field_names(&toks[open + 1..close]));
    }
    Err(CError::NoStruct {
        file: ctx.file.to_owned(),
        name: name.to_owned(),
    })
}

/// Checks a struct's fields against the column order a table is written
/// with. A field inserted or reordered upstream changes what each column
/// of the initializer means, and nothing in the values themselves would
/// say so.
pub fn check_struct<'a>(
    ctx: &Ctx<'_, 'a>,
    toks: &'a [Tok<'a>],
    name: &str,
    expected: &[&str],
) -> Result<(), CError> {
    let actual = struct_fields(ctx, toks, name)?;
    if actual != expected {
        return Err(CError::StructShape {
            file: ctx.file.to_owned(),
            name: name.to_owned(),
            expected: expected.iter().map(|f| (*f).to_owned()).collect(),
            actual: actual.iter().map(|f| (*f).to_owned()).collect(),
        });
    }
    Ok(())
}

fn matching_brace(toks: &[Tok<'_>], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (offset, tok) in toks[open..].iter().enumerate() {
        match tok.token {
            Token::Punct("{") => depth += 1,
            Token::Punct("}") => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// One name per `;`-terminated declaration: the last identifier before the
/// array bounds, if any.
fn field_names<'a>(body: &'a [Tok<'a>]) -> Vec<&'a str> {
    let mut names = Vec::new();
    let mut last = None;
    for tok in body {
        match tok.token {
            Token::Ident(name) => last = Some(name),
            Token::Punct(";") => names.extend(last.take()),
            Token::Punct("[") => names.extend(last.take()),
            Token::Punct("]") => {}
            _ => {}
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csource::lex::lex;

    fn ctx<'s, 'a>(symbols: &'s Symbols<'a>) -> Ctx<'s, 'a> {
        Ctx {
            file: "t.c",
            symbols,
        }
    }

    #[test]
    fn reads_a_nested_initializer_with_its_bounds() {
        let toks = lex(
            "t.c",
            "int checkcoord[3][4] = {\n{3,0,2,1},\n{0},\n{2,0,3,1}\n};",
        )
        .unwrap();
        let symbols = Symbols::new();
        let ctx = ctx(&symbols);
        let array = find_array(&ctx, &toks, "checkcoord").unwrap();
        assert_eq!(array.bounds, [Some(3), Some(4)]);
        let rows = array.root.list(&ctx).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].ints(&ctx, 4).unwrap(), [3, 0, 2, 1]);
        // C fills the tail of a partial initializer with zeros.
        assert_eq!(rows[1].ints(&ctx, 4).unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    fn reads_an_unbounded_array_of_strings() {
        let toks = lex("t.c", r#"char *sprnames[] = {"TROO","SHTG", NULL};"#).unwrap();
        let symbols = Symbols::new();
        let ctx = ctx(&symbols);
        let array = find_array(&ctx, &toks, "sprnames").unwrap();
        assert_eq!(array.bounds, [None]);
        let items = array.root.list(&ctx).unwrap();
        assert_eq!(items[0].text(&ctx).unwrap(), "TROO");
        assert_eq!(items[2].name(&ctx).unwrap(), "NULL");
    }

    #[test]
    fn a_string_ends_at_its_first_nul() {
        let toks = lex("t.c", r#"char *x[] = {"\0"};"#).unwrap();
        let symbols = Symbols::new();
        let ctx = ctx(&symbols);
        let array = find_array(&ctx, &toks, "x").unwrap();
        assert_eq!(array.root.list(&ctx).unwrap()[0].text(&ctx).unwrap(), "");
    }

    #[test]
    fn keeps_an_action_name_rather_than_evaluating_it() {
        let toks = lex(
            "t.c",
            "state_t states[1] = {{SPR_TROO,0,-1,{A_Punch},S_NULL}};",
        )
        .unwrap();
        let symbols = Symbols::new();
        let ctx = ctx(&symbols);
        let array = find_array(&ctx, &toks, "states").unwrap();
        let row = array.root.list(&ctx).unwrap()[0].list(&ctx).unwrap();
        assert_eq!(row[3].name(&ctx).unwrap(), "A_Punch");
        assert_eq!(row[2].int(&ctx).unwrap(), -1);
    }

    #[test]
    fn a_mention_that_is_not_a_declaration_does_not_match() {
        let toks = lex("t.c", "x = finesine[4]; const int finesine[2] = {1,2};").unwrap();
        let symbols = Symbols::new();
        let ctx = ctx(&symbols);
        let array = find_array(&ctx, &toks, "finesine").unwrap();
        assert_eq!(array.bounds, [Some(2)]);
    }

    #[test]
    fn a_missing_array_is_an_error() {
        let toks = lex("t.c", "int a[1] = {0};").unwrap();
        let symbols = Symbols::new();
        assert!(matches!(
            find_array(&ctx(&symbols), &toks, "b"),
            Err(CError::NoArray { .. })
        ));
    }

    #[test]
    fn reads_struct_field_names_in_order() {
        let toks = lex(
            "t.h",
            "typedef struct { int istexture; char endname[9]; char startname[9]; int speed; } animdef_t;",
        )
        .unwrap();
        let symbols = Symbols::new();
        let ctx = ctx(&symbols);
        assert_eq!(
            struct_fields(&ctx, &toks, "animdef_t").unwrap(),
            ["istexture", "endname", "startname", "speed"]
        );
        assert!(check_struct(&ctx, &toks, "animdef_t", &["istexture", "speed"]).is_err());
    }
}
