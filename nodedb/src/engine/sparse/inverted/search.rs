// SPDX-License-Identifier: BUSL-1.1

//! Search paths for the inverted index: BM25, phrase, fuzzy, and the
//! highlighting/offset helpers used by the SQL projection layer.

use tracing::debug;

use nodedb_fts::posting::{MatchOffset, Posting, QueryMode, TextSearchResult};
use nodedb_types::{Surrogate, TenantId};

use super::core::InvertedIndex;
use super::errors::{fts_index_err, inverted_err};
use crate::engine::sparse::fts_redb::tables::POSTINGS;

impl InvertedIndex {
    /// Search the inverted index for an exact phrase.
    ///
    /// Returns all documents where `terms` appear as a contiguous sequence in
    /// the original token stream. Positions are stored per-term in every
    /// `Posting`, so phrase matching is a set intersection on position offsets.
    ///
    /// The result is scored by position rank (earlier = higher). An optional
    /// `prefilter` bitmap restricts the candidate set before position matching.
    pub fn phrase_search(
        &self,
        tid: TenantId,
        collection: &str,
        terms: &[String],
        top_k: usize,
        prefilter: Option<&nodedb_types::SurrogateBitmap>,
    ) -> crate::Result<Vec<TextSearchResult>> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let t = tid.as_u64();
        let db = self.inner.backend().db();
        let read_txn = db.begin_read().map_err(|e| inverted_err("read txn", e))?;
        let postings_table = read_txn
            .open_table(POSTINGS)
            .map_err(|e| inverted_err("open postings", e))?;

        // Load posting list for each term.
        let mut term_lists: Vec<Vec<Posting>> = Vec::with_capacity(terms.len());
        for term in terms {
            let analyzed = nodedb_fts::analyze(term);
            let canonical = analyzed.into_iter().next().unwrap_or_else(|| term.clone());
            let postings: Vec<Posting> = postings_table
                .get((t, collection, canonical.as_str()))
                .map_err(|e| inverted_err("read posting", e))?
                .and_then(|v| zerompk::from_msgpack(v.value()).ok())
                .unwrap_or_default();
            term_lists.push(postings);
        }

        // The first term's postings are the candidate set.
        // For each candidate doc, verify remaining terms follow consecutively.
        let first = &term_lists[0];
        let mut matches: Vec<(Surrogate, u32)> = Vec::new();

        'outer: for posting in first {
            // Prefilter check.
            if prefilter.is_some_and(|bm| !bm.0.contains(posting.doc_id.as_u32())) {
                continue;
            }

            let surrogate = posting.doc_id;

            // For each start position of the first term, check subsequent terms.
            'pos: for &start_pos in &posting.positions {
                for (offset, list) in term_lists[1..].iter().enumerate() {
                    let expected_pos = start_pos + (offset as u32) + 1;
                    // Find a posting for this doc in this term's list.
                    let Some(other_posting) = list.iter().find(|p| p.doc_id == surrogate) else {
                        // Doc doesn't have this term at all — skip entire doc.
                        continue 'outer;
                    };
                    if !other_posting.positions.contains(&expected_pos) {
                        continue 'pos;
                    }
                }
                // All terms found at consecutive positions — record match.
                matches.push((surrogate, start_pos));
                break; // One match per doc is sufficient.
            }
        }

        // Sort by earliest match position (earlier = more relevant).
        matches.sort_by_key(|(_, pos)| *pos);

        let results: Vec<TextSearchResult> = matches
            .into_iter()
            .take(top_k)
            .enumerate()
            .map(|(rank, (doc_id, pos))| TextSearchResult {
                doc_id,
                score: 1.0 / (1.0 + pos as f32 + rank as f32),
                fuzzy: false,
            })
            .collect();

        debug!(
            tid = t,
            %collection,
            terms = terms.len(),
            hits = results.len(),
            "phrase search"
        );
        Ok(results)
    }

    /// Search the inverted index using BM25 scoring.
    ///
    /// Supports `NOT <term>` and `-<term>` negation in the query string.
    /// Returns `Err` for invalid queries (NOT-only, unsupported parentheses).
    pub fn search(
        &self,
        tid: TenantId,
        collection: &str,
        query: &str,
        top_k: usize,
        fuzzy_enabled: bool,
        prefilter: Option<&nodedb_types::SurrogateBitmap>,
    ) -> crate::Result<Vec<TextSearchResult>> {
        self.inner
            .search(
                tid.as_u64(),
                collection,
                query,
                top_k,
                fuzzy_enabled,
                prefilter,
            )
            .map_err(fts_index_err)
    }

    /// Search with explicit boolean mode (AND or OR).
    ///
    /// Supports `NOT <term>` and `-<term>` negation in the query string.
    #[allow(clippy::too_many_arguments)]
    pub fn search_with_mode(
        &self,
        tid: TenantId,
        collection: &str,
        query: &str,
        top_k: usize,
        fuzzy_enabled: bool,
        mode: QueryMode,
        prefilter: Option<&nodedb_types::SurrogateBitmap>,
    ) -> crate::Result<Vec<TextSearchResult>> {
        self.inner
            .search_with_mode(
                tid.as_u64(),
                collection,
                query,
                top_k,
                fuzzy_enabled,
                mode,
                prefilter,
            )
            .map_err(fts_index_err)
    }

    /// Generate highlighted text with matched query terms wrapped in tags.
    pub fn highlight(&self, text: &str, query: &str, prefix: &str, suffix: &str) -> String {
        self.inner.highlight(text, query, prefix, suffix)
    }

    /// Return byte offsets of matched query terms in the original text.
    pub fn offsets(&self, text: &str, query: &str) -> Vec<MatchOffset> {
        self.inner.offsets(text, query)
    }
}
