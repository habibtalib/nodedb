// SPDX-License-Identifier: BUSL-1.1

//! Error helpers for the inverted index module.

/// Map an `FtsIndexError<crate::Error>` to `crate::Error`.
///
/// `InvalidQuery` variants produce `crate::Error::BadRequest` so callers
/// receive a meaningful error code instead of a generic storage error.
pub(super) fn fts_index_err(e: nodedb_fts::FtsIndexError<crate::Error>) -> crate::Error {
    use nodedb_fts::FtsIndexError;
    match e {
        FtsIndexError::InvalidQuery(q) => crate::Error::BadRequest {
            detail: q.to_string(),
        },
        FtsIndexError::Backend(inner) => inner,
        other => crate::Error::Storage {
            engine: "inverted".into(),
            detail: other.to_string(),
        },
    }
}

/// Wrap a redb-side failure as `crate::Error::Storage` with engine context.
pub(super) fn inverted_err(ctx: &str, e: impl std::fmt::Display) -> crate::Error {
    crate::Error::Storage {
        engine: "inverted".into(),
        detail: format!("{ctx}: {e}"),
    }
}

/// Identity adapter so callers can `.map_err(into_result_err)` without
/// importing the type — kept as a function so future error widening (e.g.
/// adding chained context) is a single edit.
pub(super) fn into_result_err(e: crate::Error) -> crate::Error {
    e
}
