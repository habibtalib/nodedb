//! FTS query parser: recognises `NOT <term>` and `-<term>` negation operators.
//!
//! The parser is intentionally flat — parentheses are not supported. Attempting
//! `NOT (x OR y)` returns `Err(InvalidQuery::ParenthesesNotSupported)` so
//! callers get a clear message explaining the workaround.
//!
//! Grammar (informally):
//!
//! ```text
//! query  ::= token*
//! token  ::= NOT_KW term          -- negated
//!          | '-' term             -- negated (Lucene-style, no space)
//!          | term                 -- positive
//! NOT_KW ::= 'NOT' (uppercase, standalone word)
//! term   ::= any non-whitespace string that is not NOT_KW
//! ```
//!
//! Multiple negations are independent: `A NOT B NOT C` excludes both B and C.
//! A query with no positive terms (`NOT python`) returns `Err(InvalidQuery::NegativeOnly)`.

/// A parsed FTS query split into positive (required) and negative (excluded) raw terms.
///
/// Both lists are **raw** strings before analysis/stemming. The caller is
/// responsible for running each list through the collection analyzer and synonym
/// expansion before BM25 scoring and negative-bitmap construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    /// Terms that must match (positive clause).
    pub positive: Vec<String>,
    /// Terms whose matching documents are excluded from results (NOT clause).
    pub negative: Vec<String>,
}

/// Errors produced by [`parse_query`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidQuery {
    /// The query contained only negative terms with no positive terms.
    ///
    /// This is ill-defined — it would return "all documents except those
    /// matching X", which requires a full collection scan. Match PostgreSQL
    /// `tsquery` behaviour and reject the query explicitly.
    #[error(
        "FTS query must contain at least one positive term; \
         a NOT-only query (e.g. 'NOT python') is not supported"
    )]
    NegativeOnly,

    /// The query contained `NOT (...)` parenthesised groups, which are not
    /// supported in this flat-only parser.
    ///
    /// Workaround: use separate negations, e.g. `rust NOT python NOT ruby`
    /// instead of `rust NOT (python OR ruby)`.
    #[error(
        "FTS query contains parenthesised NOT groups which are not supported; \
         use flat negations instead, e.g. 'rust NOT python NOT ruby' \
         rather than 'rust NOT (python OR ruby)'"
    )]
    ParenthesesNotSupported,
}

/// Parse an FTS query string into positive and negative term lists.
///
/// Recognises:
/// - `NOT <term>` — the word `NOT` (case-sensitive) followed by a whitespace-
///   separated term marks that term as negative.
/// - `-<term>` — a term prefixed with `-` (no space) marks it as negative.
/// - Everything else is a positive term.
///
/// Returns `Err(InvalidQuery::ParenthesesNotSupported)` if a `(` is found after
/// `NOT`. Returns `Err(InvalidQuery::NegativeOnly)` if there are no positive
/// terms after parsing.
pub fn parse_query(query: &str) -> Result<ParsedQuery, InvalidQuery> {
    let mut positive = Vec::new();
    let mut negative = Vec::new();

    let tokens: Vec<&str> = query.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];

        if tok == "NOT" {
            // Consume the next token as a negated term.
            i += 1;
            if i >= tokens.len() {
                // Trailing NOT with no term — ignore it (treat as empty positive input).
                break;
            }
            let next = tokens[i];
            if next.starts_with('(') {
                return Err(InvalidQuery::ParenthesesNotSupported);
            }
            negative.push(next.to_string());
        } else if let Some(stripped) = tok.strip_prefix('-') {
            if stripped.is_empty() {
                // A bare `-` is not a negation prefix — treat as positive.
                positive.push(tok.to_string());
            } else {
                negative.push(stripped.to_string());
            }
        } else {
            positive.push(tok.to_string());
        }
        i += 1;
    }

    if positive.is_empty() && !negative.is_empty() {
        return Err(InvalidQuery::NegativeOnly);
    }

    Ok(ParsedQuery { positive, negative })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn simple_positive_terms() {
        let pq = parse_query("rust python").unwrap();
        assert_eq!(pq.positive, pos(&["rust", "python"]));
        assert!(pq.negative.is_empty());
    }

    #[test]
    fn not_keyword() {
        let pq = parse_query("rust NOT python").unwrap();
        assert_eq!(pq.positive, pos(&["rust"]));
        assert_eq!(pq.negative, pos(&["python"]));
    }

    #[test]
    fn dash_prefix() {
        let pq = parse_query("rust -python").unwrap();
        assert_eq!(pq.positive, pos(&["rust"]));
        assert_eq!(pq.negative, pos(&["python"]));
    }

    #[test]
    fn multiple_negations() {
        let pq = parse_query("rust NOT python NOT ruby").unwrap();
        assert_eq!(pq.positive, pos(&["rust"]));
        assert_eq!(pq.negative, pos(&["python", "ruby"]));
    }

    #[test]
    fn multiple_dash_negations() {
        let pq = parse_query("rust -python -ruby").unwrap();
        assert_eq!(pq.positive, pos(&["rust"]));
        assert_eq!(pq.negative, pos(&["python", "ruby"]));
    }

    #[test]
    fn negative_only_returns_error() {
        let err = parse_query("NOT python").unwrap_err();
        assert_eq!(err, InvalidQuery::NegativeOnly);
    }

    #[test]
    fn dash_only_negative_returns_error() {
        let err = parse_query("-python").unwrap_err();
        assert_eq!(err, InvalidQuery::NegativeOnly);
    }

    #[test]
    fn parentheses_after_not_returns_error() {
        let err = parse_query("rust NOT (python OR ruby)").unwrap_err();
        assert_eq!(err, InvalidQuery::ParenthesesNotSupported);
    }

    #[test]
    fn bare_dash_treated_as_positive() {
        let pq = parse_query("hello - world").unwrap();
        assert_eq!(pq.positive, pos(&["hello", "-", "world"]));
        assert!(pq.negative.is_empty());
    }

    #[test]
    fn trailing_not_ignored() {
        // Trailing NOT with no following term: positive terms are still parsed.
        let pq = parse_query("rust NOT").unwrap();
        assert_eq!(pq.positive, pos(&["rust"]));
        assert!(pq.negative.is_empty());
    }

    #[test]
    fn no_positive_only_negatives_multiple() {
        let err = parse_query("NOT python NOT ruby").unwrap_err();
        assert_eq!(err, InvalidQuery::NegativeOnly);
    }
}
