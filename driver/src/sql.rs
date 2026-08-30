//! Splitting a `.sql` file's text into the individual statements ClickHouse's
//! HTTP interface runs one at a time (unlike `clickhouse-client
//! --multiquery`, which accepts a whole file).

/// Splits `text` on top-level `;` characters: those outside a `--` line
/// comment and outside a single-quoted string. Comments and blank lines stay
/// attached to the statement that follows them. Drops an empty trailing
/// piece.
pub fn split_statements(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut in_comment = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_comment {
            if b == b'\n' {
                in_comment = false;
            }
        } else if in_string {
            if b == b'\'' {
                in_string = false;
            }
        } else {
            match b {
                b'\'' => in_string = true,
                b'-' if bytes.get(i + 1) == Some(&b'-') => in_comment = true,
                b';' => {
                    let piece = text[start..i].trim();
                    if !piece.is_empty() {
                        statements.push(piece);
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        statements.push(tail);
    }
    statements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_semicolon_inside_a_comment_does_not_split() {
        let text = "-- one; two\nSELECT 1;";
        assert_eq!(split_statements(text), vec!["-- one; two\nSELECT 1"]);
    }

    #[test]
    fn a_semicolon_inside_a_string_does_not_split() {
        let text = "SELECT 'a;b';";
        assert_eq!(split_statements(text), vec!["SELECT 'a;b'"]);
    }

    #[test]
    fn two_statements_split_apart() {
        let text = "TRUNCATE TABLE foo;\nINSERT INTO foo SELECT 1;";
        assert_eq!(
            split_statements(text),
            vec!["TRUNCATE TABLE foo", "INSERT INTO foo SELECT 1"]
        );
    }

    #[test]
    fn decode_sql_splits_into_exactly_the_truncate_and_the_insert() {
        let text = include_str!("../../sqlcpu/decode.sql");
        let statements = split_statements(text);
        assert_eq!(statements.len(), 2);
        assert!(
            statements[0]
                .trim_end()
                .ends_with("TRUNCATE TABLE IF EXISTS clickdoom.decoded")
        );
        assert!(
            statements[1]
                .trim_start()
                .starts_with("INSERT INTO clickdoom.decoded")
        );
    }
}
