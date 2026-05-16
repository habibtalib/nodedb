// SPDX-License-Identifier: BUSL-1.1

//! `ArrayOp::Aggregate` handler.
//!
//! Cross-tile reduction with optional group-by-dim. The tile-local
//! reducers in `nodedb-array::query::aggregate` produce
//! `AggregateResult` partials that merge exactly across tiles (Mean
//! carries `(sum, count)`); we fold them here and finalize once.

use std::collections::{BTreeMap, HashMap};

use nodedb_array::query::aggregate::{GroupAggregate, aggregate_attr, group_by_dim};
use nodedb_array::schema::ArraySchema;
use nodedb_array::segment::{MbrQueryPredicate, TilePayload};
use nodedb_array::types::ArrayId;
use nodedb_array::types::coord::value::CoordValue;
use nodedb_cluster::distributed_array::merge::ArrayAggPartial;
use nodedb_types::SurrogateBitmap;

use crate::bridge::envelope::{ErrorCode, Response};
use crate::data::executor::core_loop::CoreLoop;
use crate::data::executor::task::ExecutionTask;
use nodedb_physical::physical_plan::ArrayReducer;

use super::aggregate_helpers::{
    AggCell, agg_result_to_partial, apply_surrogate_filter, coord_to_agg_cell, coord_to_group_key,
    encode_agg_rows, encode_bitemporal_agg_partial, encode_partials, float_or_null, map_reducer,
    unwrap_sparse,
};

/// Aggregate query parameters bundled to avoid exceeding the 7-argument limit.
pub(in crate::data::executor) struct AggParams<'a> {
    pub array_id: &'a ArrayId,
    pub attr_idx: u32,
    pub reducer: ArrayReducer,
    pub group_by_dim_idx: i32,
    pub cell_filter: Option<&'a SurrogateBitmap>,
    pub return_partial: bool,
    /// Optional Hilbert-prefix range `[lo, hi]` for shard-level partitioning.
    pub hilbert_range: Option<(u64, u64)>,
    /// Bitemporal system-time cutoff. `None` = live read.
    pub system_as_of: Option<i64>,
    /// Bitemporal valid-time point. `None` = no valid-time filter.
    pub valid_at_ms: Option<i64>,
}

impl CoreLoop {
    pub(in crate::data::executor) fn dispatch_array_aggregate(
        &mut self,
        task: &ExecutionTask,
        p: AggParams<'_>,
    ) -> Response {
        let AggParams {
            array_id,
            attr_idx,
            reducer,
            group_by_dim_idx,
            cell_filter,
            return_partial,
            hilbert_range,
            system_as_of,
            valid_at_ms,
        } = p;
        if let Err(resp) = self.ensure_array_open(task, array_id) {
            return resp;
        }

        let schema = match self.array_engine.store(array_id) {
            Ok(store) => store.schema().clone(),
            Err(e) => {
                return self.response_error(
                    task,
                    ErrorCode::Internal {
                        detail: format!("array '{}' not open: {e}", array_id.name),
                    },
                );
            }
        };

        if system_as_of.is_some() || valid_at_ms.is_some() {
            return self.dispatch_array_aggregate_bitemporal(
                task,
                array_id,
                &schema,
                attr_idx,
                reducer,
                group_by_dim_idx,
                cell_filter,
                return_partial,
                hilbert_range,
                system_as_of,
                valid_at_ms,
            );
        }

        let all_tiles_with_prefix = match self
            .array_engine
            .scan_tiles_with_hilbert_prefix(array_id, &MbrQueryPredicate::default())
        {
            Ok(t) => t,
            Err(e) => {
                return self.response_error(
                    task,
                    ErrorCode::Internal {
                        detail: format!("array aggregate scan: {e}"),
                    },
                );
            }
        };

        let all_tiles: Vec<TilePayload> = match hilbert_range {
            Some((lo, hi)) => all_tiles_with_prefix
                .into_iter()
                .filter_map(|(hp, tile)| {
                    if hp >= lo && hp <= hi {
                        Some(tile)
                    } else {
                        None
                    }
                })
                .collect(),
            None => all_tiles_with_prefix
                .into_iter()
                .map(|(_, tile)| tile)
                .collect(),
        };

        let r = map_reducer(reducer);
        let attr = attr_idx as usize;

        if group_by_dim_idx < 0 {
            let mut acc = None;
            for tile in all_tiles {
                let sparse = match unwrap_sparse(tile) {
                    Ok(s) => s,
                    Err(code) => return self.response_error(task, code),
                };
                let sparse = match apply_surrogate_filter(&schema, sparse, cell_filter) {
                    Ok(s) => s,
                    Err(code) => return self.response_error(task, code),
                };
                let part = aggregate_attr(&sparse, attr, r);
                acc = Some(match acc {
                    Some(prev) => {
                        nodedb_array::query::aggregate::AggregateResult::merge(prev, part)
                    }
                    None => part,
                });
            }
            if return_partial {
                let partial =
                    acc.map(|a| agg_result_to_partial(0, a))
                        .unwrap_or_else(|| ArrayAggPartial {
                            group_key: 0,
                            count: 0,
                            sum: 0.0,
                            min: f64::INFINITY,
                            max: f64::NEG_INFINITY,
                            welford_mean: 0.0,
                            welford_m2: 0.0,
                        });
                return encode_partials(self, task, &[partial]);
            }
            let final_val = acc.and_then(|a| a.finalize());
            let mut row: BTreeMap<&'static str, AggCell> = BTreeMap::new();
            row.insert("result", float_or_null(final_val));
            return encode_agg_rows(self, task, &[row]);
        }

        let dim = group_by_dim_idx as usize;
        let mut order: Vec<CoordValue> = Vec::new();
        let mut by_key: HashMap<CoordValue, nodedb_array::query::aggregate::AggregateResult> =
            HashMap::new();
        for tile in all_tiles {
            let sparse = match unwrap_sparse(tile) {
                Ok(s) => s,
                Err(code) => return self.response_error(task, code),
            };
            let sparse = match apply_surrogate_filter(&schema, sparse, cell_filter) {
                Ok(s) => s,
                Err(code) => return self.response_error(task, code),
            };
            let groups: Vec<GroupAggregate> = group_by_dim(&sparse, dim, attr, r);
            for g in groups {
                match by_key.get_mut(&g.key) {
                    Some(prev) => *prev = prev.merge(g.result),
                    None => {
                        order.push(g.key.clone());
                        by_key.insert(g.key, g.result);
                    }
                }
            }
        }

        if return_partial {
            let partials: Vec<ArrayAggPartial> = order
                .iter()
                .filter_map(|key| {
                    by_key
                        .remove(key)
                        .map(|agg| agg_result_to_partial(coord_to_group_key(key), agg))
                })
                .collect();
            return encode_partials(self, task, &partials);
        }

        let mut rows: Vec<BTreeMap<&'static str, AggCell>> = Vec::with_capacity(order.len());
        for key in order {
            let result_val = by_key.remove(&key).and_then(|r| r.finalize());
            let mut row: BTreeMap<&'static str, AggCell> = BTreeMap::new();
            row.insert("group", coord_to_agg_cell(&key));
            row.insert("result", float_or_null(result_val));
            rows.push(row);
        }
        encode_agg_rows(self, task, &rows)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_array_aggregate_bitemporal(
        &mut self,
        task: &ExecutionTask,
        array_id: &ArrayId,
        schema: &ArraySchema,
        attr_idx: u32,
        reducer: ArrayReducer,
        group_by_dim_idx: i32,
        cell_filter: Option<&SurrogateBitmap>,
        return_partial: bool,
        hilbert_range: Option<(u64, u64)>,
        system_as_of: Option<i64>,
        valid_at_ms: Option<i64>,
    ) -> Response {
        let cutoff = system_as_of.unwrap_or(i64::MAX);
        let store = match self.array_engine.store(array_id) {
            Ok(s) => s,
            Err(e) => {
                return self.response_error(
                    task,
                    ErrorCode::Internal {
                        detail: format!("array '{}' not open: {e}", array_id.name),
                    },
                );
            }
        };
        let (resolved_tiles, truncated_before_horizon) =
            match store.scan_tiles_at(cutoff, valid_at_ms) {
                Ok(r) => r,
                Err(e) => {
                    return self.response_error(
                        task,
                        ErrorCode::Internal {
                            detail: format!("array bitemporal aggregate scan: {e}"),
                        },
                    );
                }
            };

        let r = map_reducer(reducer);
        let attr = attr_idx as usize;

        let all_tiles: Vec<TilePayload> = resolved_tiles
            .into_iter()
            .filter(|(hp, _)| match hilbert_range {
                Some((lo, hi)) => *hp >= lo && *hp <= hi,
                None => true,
            })
            .map(|(_, tile)| TilePayload::Sparse(tile))
            .collect();

        if group_by_dim_idx < 0 {
            let mut acc = None;
            for tile in all_tiles {
                let sparse = match unwrap_sparse(tile) {
                    Ok(s) => s,
                    Err(code) => return self.response_error(task, code),
                };
                let sparse = match apply_surrogate_filter(schema, sparse, cell_filter) {
                    Ok(s) => s,
                    Err(code) => return self.response_error(task, code),
                };
                let part = aggregate_attr(&sparse, attr, r);
                acc = Some(match acc {
                    Some(prev) => {
                        nodedb_array::query::aggregate::AggregateResult::merge(prev, part)
                    }
                    None => part,
                });
            }
            if return_partial {
                let partial =
                    acc.map(|a| agg_result_to_partial(0, a))
                        .unwrap_or_else(|| ArrayAggPartial {
                            group_key: 0,
                            count: 0,
                            sum: 0.0,
                            min: f64::INFINITY,
                            max: f64::NEG_INFINITY,
                            welford_mean: 0.0,
                            welford_m2: 0.0,
                        });
                return encode_bitemporal_agg_partial(
                    self,
                    task,
                    &[partial],
                    truncated_before_horizon,
                );
            }
            let final_val = acc.and_then(|a| a.finalize());
            let mut row: BTreeMap<&'static str, AggCell> = BTreeMap::new();
            row.insert("result", float_or_null(final_val));
            row.insert(
                "truncated_before_horizon",
                AggCell::Bool(truncated_before_horizon),
            );
            return encode_agg_rows(self, task, &[row]);
        }

        let dim = group_by_dim_idx as usize;
        let mut order: Vec<CoordValue> = Vec::new();
        let mut by_key: HashMap<CoordValue, nodedb_array::query::aggregate::AggregateResult> =
            HashMap::new();
        for tile in all_tiles {
            let sparse = match unwrap_sparse(tile) {
                Ok(s) => s,
                Err(code) => return self.response_error(task, code),
            };
            let sparse = match apply_surrogate_filter(schema, sparse, cell_filter) {
                Ok(s) => s,
                Err(code) => return self.response_error(task, code),
            };
            let groups: Vec<GroupAggregate> = group_by_dim(&sparse, dim, attr, r);
            for g in groups {
                match by_key.get_mut(&g.key) {
                    Some(prev) => *prev = prev.merge(g.result),
                    None => {
                        order.push(g.key.clone());
                        by_key.insert(g.key, g.result);
                    }
                }
            }
        }
        if return_partial {
            let partials: Vec<ArrayAggPartial> = order
                .iter()
                .filter_map(|key| {
                    by_key
                        .remove(key)
                        .map(|agg| agg_result_to_partial(coord_to_group_key(key), agg))
                })
                .collect();
            return encode_bitemporal_agg_partial(self, task, &partials, truncated_before_horizon);
        }
        let mut rows: Vec<BTreeMap<&'static str, AggCell>> = Vec::with_capacity(order.len());
        for key in order {
            let result_val = by_key.remove(&key).and_then(|r| r.finalize());
            let mut row: BTreeMap<&'static str, AggCell> = BTreeMap::new();
            row.insert("group", coord_to_agg_cell(&key));
            row.insert("result", float_or_null(result_val));
            rows.push(row);
        }
        let mut summary: BTreeMap<&'static str, AggCell> = BTreeMap::new();
        summary.insert(
            "truncated_before_horizon",
            AggCell::Bool(truncated_before_horizon),
        );
        rows.push(summary);
        encode_agg_rows(self, task, &rows)
    }
}
