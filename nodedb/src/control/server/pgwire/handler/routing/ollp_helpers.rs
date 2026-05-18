// SPDX-License-Identifier: BUSL-1.1

//! Helper functions for the OLLP dependent-read dispatch path.
//!
//! These helpers are called from `execute_planned_sql_inner` when a
//! multi-shard strict transaction contains a value-dependent predicate.

use nodedb_physical::physical_plan::{DocumentOp, PhysicalPlan};

/// Extract the collection name and serialized filter bytes from a
/// `BulkUpdate` or `BulkDelete` plan.
///
/// Returns `("", vec![])` for plan variants that are not bulk predicates.
pub(super) fn extract_bulk_predicate_info(plan: &PhysicalPlan) -> (String, Vec<u8>) {
    match plan {
        PhysicalPlan::Document(DocumentOp::BulkUpdate {
            collection,
            filters,
            ..
        })
        | PhysicalPlan::Document(DocumentOp::BulkDelete {
            collection,
            filters,
            ..
        }) => (collection.clone(), filters.clone()),
        _ => (String::new(), vec![]),
    }
}

/// Inject `ollp_predicted_surrogates` into a `BulkUpdate` or `BulkDelete`
/// plan in-place.
///
/// Other plan variants are left unchanged. Idempotent — calling twice
/// replaces the previous prediction with the new one.
pub(super) fn inject_ollp_surrogates(plan: &mut PhysicalPlan, surrogates: Vec<u32>) {
    match plan {
        PhysicalPlan::Document(DocumentOp::BulkUpdate {
            ollp_predicted_surrogates,
            ..
        })
        | PhysicalPlan::Document(DocumentOp::BulkDelete {
            ollp_predicted_surrogates,
            ..
        }) => {
            *ollp_predicted_surrogates = Some(surrogates);
        }
        _ => {}
    }
}
