// SPDX-License-Identifier: BUSL-1.1

//! Dispatch for SpatialOp variants (scan, insert, delete).

use crate::bridge::envelope::Response;
use crate::bridge::physical_plan::SpatialOp;

use crate::data::executor::core_loop::CoreLoop;
use crate::data::executor::task::ExecutionTask;

impl CoreLoop {
    pub(super) fn dispatch_spatial(
        &mut self,
        task: &ExecutionTask,
        tid: u64,
        op: &SpatialOp,
    ) -> Response {
        match op {
            SpatialOp::Insert {
                collection,
                field,
                surrogate,
                geometry,
            } => self.execute_spatial_insert(task, tid, collection, field, *surrogate, geometry),

            SpatialOp::Delete {
                collection,
                field,
                surrogate,
            } => self.execute_spatial_delete(task, tid, collection, field, *surrogate),

            SpatialOp::Scan {
                collection,
                field,
                predicate,
                query_geometry,
                distance_meters,
                attribute_filters,
                limit,
                projection,
                rls_filters,
                prefilter,
            } => self.execute_spatial_scan(
                task,
                tid,
                collection,
                field,
                predicate,
                query_geometry,
                *distance_meters,
                attribute_filters,
                *limit,
                projection,
                rls_filters,
                prefilter.as_ref(),
            ),
        }
    }
}
