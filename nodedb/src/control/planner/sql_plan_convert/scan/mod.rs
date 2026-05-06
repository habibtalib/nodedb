//! Scan and search plan conversions (read-only query paths).
//!
//! Split by concern so each file stays under the project's hard size limit.

mod core;
mod helpers;
mod join;
mod recursive;
mod search;
mod spatial;
mod timeseries;

pub(in crate::control::planner::sql_plan_convert) use core::{
    convert_document_index_lookup, convert_point_get, convert_scan,
};
pub(in crate::control::planner::sql_plan_convert) use join::convert_join;
pub(in crate::control::planner::sql_plan_convert) use recursive::convert_recursive_scan;
pub(in crate::control::planner::sql_plan_convert) use search::{
    convert_hybrid_search, convert_hybrid_search_triple, convert_text_search, convert_vector_search,
};
pub(in crate::control::planner::sql_plan_convert) use spatial::convert_spatial_scan;
pub(in crate::control::planner::sql_plan_convert) use timeseries::{
    convert_timeseries_ingest, convert_timeseries_scan,
};
