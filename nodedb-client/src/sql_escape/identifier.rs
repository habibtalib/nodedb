// SPDX-License-Identifier: Apache-2.0

//! SQL identifier escaping.

/// Quote a SQL identifier (collection / column name) by doubling any
/// internal double-quotes and wrapping the result in double-quotes —
/// the SQL standard rule that PostgreSQL applies under
/// `standard_conforming_strings=on`.
pub(crate) fn quote_identifier(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_escapes_double_quotes() {
        assert_eq!(quote_identifier("foo"), "\"foo\"");
        // Embedded `"` must be doubled per the SQL identifier-escape rule.
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
    }
}
