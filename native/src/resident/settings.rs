//! The settings a resident statement runs under.
//!
//! They travel as URL parameters rather than a `SETTINGS` clause, because
//! the server has to know them before it parses the statement.

/// The bytes `max_query_size` allows above the statement's own length.
/// `NATIVE.md` fixes the value. It has to cover the format clause the
/// transport appends to the statement and the newline after it.
pub const QUERY_SIZE_SLACK: usize = 64;

/// Every setting a resident statement needs, for a statement of
/// `statement_bytes` bytes.
///
/// One streamed row is one block, processed as it arrives and visible to
/// the next row: that is what the block-size and threading settings buy.
/// `max_query_size` covers the statement text, which leads the request
/// body.
pub fn resident_settings(statement_bytes: usize) -> Vec<(&'static str, String)> {
    vec![
        ("max_insert_block_size", "1".to_owned()),
        ("min_insert_block_size_rows", "1".to_owned()),
        ("min_insert_block_size_bytes", "1".to_owned()),
        ("input_format_parallel_parsing", "0".to_owned()),
        ("max_block_size", "1".to_owned()),
        ("max_threads", "1".to_owned()),
        ("max_insert_threads", "1".to_owned()),
        ("async_insert", "0".to_owned()),
        (
            "max_query_size",
            (statement_bytes + QUERY_SIZE_SLACK).to_string(),
        ),
        ("max_ast_elements", "4000000".to_owned()),
        ("max_expanded_ast_elements", "40000000".to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value<'a>(settings: &'a [(&str, String)], name: &str) -> &'a str {
        settings
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("{name} is not in the settings"))
    }

    #[test]
    fn every_row_is_its_own_block() {
        let settings = resident_settings(0);
        for name in [
            "max_insert_block_size",
            "min_insert_block_size_rows",
            "min_insert_block_size_bytes",
            "max_block_size",
        ] {
            assert_eq!(value(&settings, name), "1", "{name}");
        }
    }

    #[test]
    fn rows_are_processed_in_order_and_synchronously() {
        let settings = resident_settings(0);
        assert_eq!(value(&settings, "max_threads"), "1");
        assert_eq!(value(&settings, "max_insert_threads"), "1");
        assert_eq!(value(&settings, "input_format_parallel_parsing"), "0");
        assert_eq!(value(&settings, "async_insert"), "0");
    }

    #[test]
    fn max_query_size_clears_the_statement() {
        assert_eq!(value(&resident_settings(0), "max_query_size"), "64");
        assert_eq!(
            value(&resident_settings(200_000), "max_query_size"),
            "200064"
        );
    }

    #[test]
    fn no_setting_is_named_twice() {
        let settings = resident_settings(1);
        let mut names: Vec<&str> = settings.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "a setting is listed twice");
    }
}
