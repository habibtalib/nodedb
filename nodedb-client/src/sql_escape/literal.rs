// SPDX-License-Identifier: Apache-2.0

//! SQL string-literal escaping.

/// Render a `&str` as a SQL string literal: single-quote-doubled and
/// wrapped in single quotes. Matches `standard_conforming_strings=on`
/// behavior (PG 9.1+ default), the only mode the server runs in.
///
/// Centralizes the escape so call sites can't drift into raw `format!`s
/// without going through it — every `'foo'` written into a SQL string
/// inside this crate goes through here.
pub(crate) fn quote_string_literal(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_single_quotes() {
        assert_eq!(quote_string_literal("plain"), "'plain'");
        // The PG standard rule under `standard_conforming_strings=on`:
        // double every embedded `'`. A `O'Reilly` literal that lost its
        // escape would close the SQL string early and inject the rest.
        assert_eq!(quote_string_literal("O'Reilly"), "'O''Reilly'");
        assert_eq!(
            quote_string_literal("'; DROP TABLE x; --"),
            "'''; DROP TABLE x; --'"
        );
    }

    #[test]
    fn passes_through_json() {
        // The metadata path renders sonic_rs JSON and then quotes it as
        // a SQL string. JSON already escapes its own `"` and `\`, so
        // only the outer `'` needs SQL escaping.
        let json = r#"{"name":"O'Reilly","ok":true}"#;
        let quoted = quote_string_literal(json);
        assert_eq!(quoted, "'{\"name\":\"O''Reilly\",\"ok\":true}'");
    }
}
