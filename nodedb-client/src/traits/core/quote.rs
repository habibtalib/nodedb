// SPDX-License-Identifier: Apache-2.0

//! SQL identifier quoting helper used by `NodeDb` trait default impls.

/// Quote a SQL identifier. Wraps in double-quotes only if the name
/// contains anything other than `[A-Za-z0-9_]` or starts with a digit —
/// the unquoted fast-path keeps the usual case cheap. Doubles any
/// internal double-quotes per the SQL identifier-escape rule.
///
/// Lives next to the trait default impls (rather than in the remote
/// client's `quote_identifier`) because the trait defaults for
/// `undrop_collection` / `drop_collection_purge` build SQL without any
/// feature-gated transport in scope.
pub(crate) fn quote_ident(name: &str) -> String {
    let needs_quote = name.is_empty()
        || name.chars().next().is_some_and(|c| c.is_ascii_digit())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if needs_quote {
        let escaped = name.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        name.to_string()
    }
}
