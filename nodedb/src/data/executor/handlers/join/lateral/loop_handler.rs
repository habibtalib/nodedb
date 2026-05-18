// SPDX-License-Identifier: BUSL-1.1

//! `LateralLoop` handler: general nested-loop LATERAL for non-equi correlations.

use tracing::debug;

use crate::bridge::envelope::{ErrorCode, PhysicalPlan, Response};
use crate::data::executor::core_loop::CoreLoop;
use crate::data::executor::response_codec;
use crate::data::executor::task::ExecutionTask;
use nodedb_physical::physical_plan::JoinProjection;

use super::shared::{
    MAX_RESULT_ROWS, bind_outer_values, build_row, build_scan_plan, extract_outer_field,
    flatten_outer_row, unwrap_data_field,
};
use crate::bridge::scan_filter::{FilterOp, ScanFilter};

/// Parameters for `LateralLoop` execution.
pub struct LateralLoopParams<'a> {
    pub task: &'a ExecutionTask,
    pub tid: u64,
    pub outer_plan: &'a PhysicalPlan,
    pub outer_alias: &'a str,
    pub inner_collection: &'a str,
    pub inner_filters: &'a [u8],
    pub correlation_predicates: &'a [(String, String)],
    pub lateral_alias: &'a str,
    pub projection: &'a [JoinProjection],
    pub left_join: bool,
    pub outer_row_cap: usize,
}

impl CoreLoop {
    pub(in crate::data::executor) fn execute_lateral_loop(
        &mut self,
        p: LateralLoopParams<'_>,
    ) -> Response {
        debug!(
            core = self.core_id,
            inner = %p.inner_collection,
            cap = p.outer_row_cap,
            "lateral loop"
        );

        let outer_resp = self.execute_plan(p.task, p.outer_plan);
        let outer_docs = match response_codec::decode_response_to_docs(&outer_resp) {
            Some(docs) => docs,
            None => return outer_resp,
        };

        if outer_docs.len() > p.outer_row_cap {
            return self.response_error(
                p.task,
                ErrorCode::Unsupported {
                    detail: format!(
                        "LATERAL query exceeded outer-row cap of {} rows; \
                         use a more selective outer filter",
                        p.outer_row_cap
                    ),
                },
            );
        }

        let base_inner_filters: Vec<ScanFilter> = if p.inner_filters.is_empty() {
            Vec::new()
        } else {
            zerompk::from_msgpack(p.inner_filters).unwrap_or_default()
        };

        let mut result: Vec<Vec<u8>> = Vec::new();

        for (_outer_id, outer_bytes) in &outer_docs {
            // Produce a flat outer map that includes wrapper-level fields (e.g.
            // the primary-key `id`) alongside the document payload fields.
            let flat_outer = flatten_outer_row(outer_bytes);

            // Bind any *Column filter ops (e.g. GtColumn for non-equi correlations)
            // that reference the outer row by substituting the actual runtime value.
            let bound_filters =
                bind_outer_values(base_inner_filters.clone(), outer_bytes, p.outer_alias);

            let mut corr_filters = bound_filters;
            for (inner_col, outer_col) in p.correlation_predicates {
                if let Some(val) = extract_outer_field(outer_bytes, outer_col) {
                    corr_filters.push(ScanFilter {
                        field: inner_col.clone(),
                        op: FilterOp::Eq,
                        value: val,
                        clauses: Vec::new(),
                        expr: None,
                    });
                }
            }

            let filter_bytes = match zerompk::to_msgpack_vec(&corr_filters) {
                Ok(b) => b,
                Err(e) => {
                    return self.response_error(
                        p.task,
                        ErrorCode::Internal {
                            detail: format!("lateral loop filter serialization: {e}"),
                        },
                    );
                }
            };

            let inner_plan = build_scan_plan(p.inner_collection, filter_bytes, &[], usize::MAX);

            let inner_resp = self.execute_plan(p.task, &inner_plan);
            let inner_docs = response_codec::decode_response_to_docs(&inner_resp);

            match inner_docs {
                None if p.left_join => {
                    let row = build_row(
                        &flat_outer,
                        None,
                        p.outer_alias,
                        p.lateral_alias,
                        p.projection,
                    );
                    result.push(row);
                }
                None => {}
                Some(docs) if docs.is_empty() && p.left_join => {
                    let row = build_row(
                        &flat_outer,
                        None,
                        p.outer_alias,
                        p.lateral_alias,
                        p.projection,
                    );
                    result.push(row);
                }
                Some(docs) => {
                    for (_inner_id, inner_bytes) in &docs {
                        let inner_data = unwrap_data_field(inner_bytes);
                        let row = build_row(
                            &flat_outer,
                            Some(inner_data),
                            p.outer_alias,
                            p.lateral_alias,
                            p.projection,
                        );
                        result.push(row);
                        if result.len() >= MAX_RESULT_ROWS {
                            break;
                        }
                    }
                }
            }
            if result.len() >= MAX_RESULT_ROWS {
                break;
            }
        }

        let payload = response_codec::encode_binary_rows(&result);
        self.response_with_payload(p.task, payload)
    }
}
