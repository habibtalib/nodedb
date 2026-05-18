// SPDX-License-Identifier: BUSL-1.1
//! `ConvertVisitor` — implements `PlanVisitor` to lower SqlPlan → PhysicalTask.
//! Method bodies live in sibling `arms_*.rs` files (macro_rules), grouped by family.

use nodedb_sql::PlanVisitor;

use crate::types::TenantId;
use nodedb_physical::physical_task::PhysicalTask;

use super::super::convert::ConvertContext;
use super::arms_aggregate_lateral::impl_aggregate_lateral_arms_for_convert_visitor;
use super::arms_array::impl_array_arms_for_convert_visitor;
use super::arms_dml::impl_dml_arms_for_convert_visitor;
use super::arms_scan_read::impl_scan_read_arms_for_convert_visitor;
use super::arms_scan_search::impl_scan_search_arms_for_convert_visitor;
use super::arms_set_ops::impl_set_ops_arms_for_convert_visitor;
use super::unsupported_arms::impl_unsupported_convert_visitor_methods;

pub struct ConvertVisitor<'a> {
    pub tenant_id: TenantId,
    pub ctx: &'a ConvertContext,
}

impl<'a> PlanVisitor for ConvertVisitor<'a> {
    type Output = Vec<PhysicalTask>;
    type Error = crate::Error;

    impl_scan_read_arms_for_convert_visitor!();
    impl_scan_search_arms_for_convert_visitor!();
    impl_dml_arms_for_convert_visitor!();
    impl_set_ops_arms_for_convert_visitor!();
    impl_aggregate_lateral_arms_for_convert_visitor!();
    impl_array_arms_for_convert_visitor!();
    impl_unsupported_convert_visitor_methods!();
}
