// SPDX-License-Identifier: BUSL-1.1

//! Text (FTS) operation dispatch.

use crate::bridge::envelope::Response;
use crate::bridge::physical_plan::TextOp;

use crate::data::executor::core_loop::CoreLoop;
use crate::data::executor::task::ExecutionTask;

impl CoreLoop {
    pub(super) fn dispatch_text(&mut self, task: &ExecutionTask, op: &TextOp) -> Response {
        let tid = task.request.tenant_id.as_u64();
        match op {
            TextOp::Search {
                collection,
                query,
                top_k,
                fuzzy,
                prefilter,
                rls_filters,
            } => self.execute_text_search(
                task,
                tid,
                collection,
                query,
                *top_k,
                *fuzzy,
                prefilter.as_ref(),
                rls_filters,
            ),

            TextOp::BM25ScoreScan {
                collection,
                query,
                score_alias,
                fuzzy,
            } => self.execute_bm25_score_scan(task, tid, collection, query, score_alias, *fuzzy),

            TextOp::PhraseSearch {
                collection,
                terms,
                top_k,
                prefilter,
            } => {
                self.execute_phrase_search(task, tid, collection, terms, *top_k, prefilter.as_ref())
            }

            TextOp::HybridSearch {
                collection,
                query_vector,
                query_text,
                top_k,
                ef_search,
                fuzzy,
                vector_weight,
                filter_bitmap,
                rls_filters,
                score_alias,
            } => self.execute_hybrid_search(
                task,
                tid,
                collection,
                query_vector,
                query_text,
                *top_k,
                *ef_search,
                *fuzzy,
                *vector_weight,
                filter_bitmap.as_ref(),
                rls_filters,
                score_alias.as_deref(),
            ),

            TextOp::FtsIndexDoc {
                collection,
                surrogate,
                text,
            } => {
                use nodedb_types::TenantId;

                let tenant_id = TenantId::new(tid);
                match self
                    .inverted
                    .index_document(tenant_id, collection, *surrogate, text)
                {
                    Ok(()) => self.response_ok(task),
                    Err(e) => {
                        tracing::warn!(
                            core = self.core_id,
                            %collection,
                            surrogate = surrogate.as_u32(),
                            error = %e,
                            "FtsIndexDoc: inverted index write failed"
                        );
                        self.response_error(task, e)
                    }
                }
            }

            TextOp::FtsDeleteDoc {
                collection,
                surrogate,
            } => {
                use nodedb_types::TenantId;

                let tenant_id = TenantId::new(tid);
                match self
                    .inverted
                    .remove_document(tenant_id, collection, *surrogate)
                {
                    Ok(()) => self.response_ok(task),
                    Err(e) => {
                        tracing::warn!(
                            core = self.core_id,
                            %collection,
                            surrogate = surrogate.as_u32(),
                            error = %e,
                            "FtsDeleteDoc: inverted index removal failed"
                        );
                        self.response_error(task, e)
                    }
                }
            }

            TextOp::HybridSearchTriple {
                collection,
                query_vector,
                query_text,
                graph_seed_id,
                graph_depth,
                graph_edge_label,
                top_k,
                ef_search,
                fuzzy,
                rrf_k,
                filter_bitmap,
                rls_filters,
                score_alias,
            } => self.execute_hybrid_search_triple(
                task,
                tid,
                collection,
                query_vector,
                query_text,
                graph_seed_id,
                *graph_depth,
                graph_edge_label.as_deref(),
                *top_k,
                *ef_search,
                *fuzzy,
                *rrf_k,
                filter_bitmap.as_ref(),
                rls_filters,
                score_alias.as_deref(),
            ),
        }
    }
}
