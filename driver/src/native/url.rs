//! The request URL a resident statement is opened with.

/// The path and query string for a resident statement: the database, then
/// every setting, in the order given.
pub fn request_target(database: &str, settings: &[(&str, String)]) -> String {
    let mut target = String::from("/?database=");
    encode_into(&mut target, database);
    for (name, value) in settings {
        target.push('&');
        encode_into(&mut target, name);
        target.push('=');
        encode_into(&mut target, value);
    }
    target
}

/// Appends `text` percent-encoded: unreserved characters as themselves,
/// everything else as `%XX` of its bytes.
fn encode_into(target: &mut String, text: &str) {
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                target.push(byte as char);
            }
            _ => target.push_str(&format!("%{byte:02X}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
        pairs
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect()
    }

    #[test]
    fn the_database_comes_first_then_the_settings_in_order() {
        let target = request_target(
            "clickdoom",
            &settings(&[("max_threads", "1"), ("async_insert", "0")]),
        );
        assert_eq!(target, "/?database=clickdoom&max_threads=1&async_insert=0");
    }

    #[test]
    fn a_setting_value_that_is_not_unreserved_is_percent_encoded() {
        let target = request_target("db", &settings(&[("query_id", "native sim/17&x")]));
        assert_eq!(target, "/?database=db&query_id=native%20sim%2F17%26x");
    }

    #[test]
    fn unreserved_characters_are_left_alone() {
        let target = request_target("a-z_0.9~", &[]);
        assert_eq!(target, "/?database=a-z_0.9~");
    }

    #[test]
    fn a_multibyte_character_is_encoded_byte_by_byte() {
        let target = request_target("é", &[]);
        assert_eq!(target, "/?database=%C3%A9");
    }
}
