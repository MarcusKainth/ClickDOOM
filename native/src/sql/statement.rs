//! One statement, and how a script splits into statements.

/// A statement to issue, and the request body it carries.
///
/// A body is empty for everything but an `INSERT ... FORMAT`, whose rows
/// travel outside the statement text. A caller issues these in order and
/// does nothing else.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Statement {
    pub sql: String,
    pub body: Vec<u8>,
}

impl Statement {
    pub fn sql(sql: impl Into<String>) -> Statement {
        Statement {
            sql: sql.into(),
            body: Vec::new(),
        }
    }

    pub fn data(sql: impl Into<String>, body: Vec<u8>) -> Statement {
        Statement {
            sql: sql.into(),
            body,
        }
    }

    /// A one-line description: the statement's first line, and the body's
    /// size when it has one. Long enough to identify a statement, short
    /// enough to compare a whole plan against a golden file.
    pub fn summary(&self) -> String {
        let head = self.sql.lines().next().unwrap_or_default().trim();
        match self.body.len() {
            0 => head.to_owned(),
            len => format!("{head}  [{len} bytes]"),
        }
    }
}

/// Splits `script` on the `;` outside a `--` comment and outside a quoted
/// string. Drops the empty pieces.
///
/// `driver/src/sql.rs` does the same split for the driver and carries the
/// tests for it. This crate does not depend on the driver.
pub fn split_statements(script: &str) -> Vec<&str> {
    let bytes = script.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut in_comment = false;
    for (at, b) in bytes.iter().enumerate() {
        if in_comment {
            in_comment = *b != b'\n';
        } else if in_string {
            in_string = *b != b'\'';
        } else {
            match b {
                b'\'' => in_string = true,
                b'-' if bytes.get(at + 1) == Some(&b'-') => in_comment = true,
                b';' => {
                    push_trimmed(&mut statements, &script[start..at]);
                    start = at + 1;
                }
                _ => {}
            }
        }
    }
    push_trimmed(&mut statements, &script[start..]);
    statements
}

/// Adds a statement with its surrounding blank lines and leading comment
/// lines removed. A comment before a statement belongs to the file, not to
/// the statement the server sees.
fn push_trimmed<'a>(statements: &mut Vec<&'a str>, piece: &'a str) {
    let mut body = piece.trim();
    while let Some(rest) = body.strip_prefix("--") {
        body = rest.split_once('\n').map_or("", |(_, tail)| tail).trim();
    }
    if !body.is_empty() {
        statements.push(body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_the_semicolons_between_statements() {
        assert_eq!(
            split_statements("CREATE A; CREATE B;\n\nCREATE C"),
            ["CREATE A", "CREATE B", "CREATE C"]
        );
    }

    #[test]
    fn a_semicolon_in_a_comment_or_a_string_does_not_split() {
        assert_eq!(
            split_statements("SELECT 1 -- a; b\n; SELECT ';'"),
            ["SELECT 1 -- a; b", "SELECT ';'"]
        );
    }

    #[test]
    fn a_comment_before_a_statement_is_dropped() {
        let script = "-- why\n-- more\nCREATE A;\n\n-- next\nCREATE B (x -- inline\n);";
        assert_eq!(
            split_statements(script),
            ["CREATE A", "CREATE B (x -- inline\n)"]
        );
    }

    #[test]
    fn a_summary_names_the_statement_and_its_body_size() {
        assert_eq!(Statement::sql("CREATE A\n(x)").summary(), "CREATE A");
        assert_eq!(
            Statement::data("INSERT INTO t FORMAT TSV", vec![1, 2, 3]).summary(),
            "INSERT INTO t FORMAT TSV  [3 bytes]"
        );
    }
}
