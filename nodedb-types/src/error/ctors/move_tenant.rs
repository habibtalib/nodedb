// SPDX-License-Identifier: Apache-2.0

//! `MOVE TENANT` error constructors (codes 1600–1604).

use super::super::code::ErrorCode;
use super::super::details::ErrorDetails;
use super::super::types::NodeDbError;

impl NodeDbError {
    /// Drain phase timed out; the tenant's sessions on source could not be
    /// cleanly wound down within the bounded window. The source is left
    /// unmodified; no data was moved.
    pub fn move_tenant_drain_timeout(
        tenant: impl Into<String>,
        source_db: impl Into<String>,
    ) -> Self {
        let tenant = tenant.into();
        let source_db = source_db.into();
        Self {
            code: ErrorCode::MOVE_TENANT_DRAIN_TIMEOUT,
            message: format!(
                "MOVE TENANT '{tenant}': drain timeout on source database '{source_db}'; \
                 no data was moved — retry after ensuring the tenant has no active \
                 connections on the source"
            ),
            details: ErrorDetails::MoveTenantDrainTimeout { tenant, source_db },
            cause: None,
        }
    }

    /// Pre-flight schema-compatibility check failed. No state was mutated.
    pub fn move_tenant_preflight_failed(
        tenant: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let tenant = tenant.into();
        let detail = detail.into();
        Self {
            code: ErrorCode::MOVE_TENANT_PREFLIGHT_FAILED,
            message: format!("MOVE TENANT '{tenant}' pre-flight failed: {detail}"),
            details: ErrorDetails::MoveTenantPreflightFailed { tenant, detail },
            cause: None,
        }
    }

    /// Snapshot phase failed; partial snapshot has been cleaned up. Source
    /// is unmodified.
    pub fn move_tenant_snapshot_failed(
        tenant: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let tenant = tenant.into();
        let detail = detail.into();
        Self {
            code: ErrorCode::MOVE_TENANT_SNAPSHOT_FAILED,
            message: format!("MOVE TENANT '{tenant}' snapshot failed: {detail}"),
            details: ErrorDetails::MoveTenantSnapshotFailed { tenant, detail },
            cause: None,
        }
    }

    /// Cutover Raft proposal failed. The source database still holds the
    /// tenant's data; no partial state was left.
    pub fn move_tenant_cutover_failed(
        tenant: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let tenant = tenant.into();
        let detail = detail.into();
        Self {
            code: ErrorCode::MOVE_TENANT_CUTOVER_FAILED,
            message: format!(
                "MOVE TENANT '{tenant}' cutover failed: {detail}; \
                 source database is intact"
            ),
            details: ErrorDetails::MoveTenantCutoverFailed { tenant, detail },
            cause: None,
        }
    }

    /// The tenant's data is already in the target database; this is the
    /// idempotent response when a previously completed `MOVE TENANT` is
    /// re-issued.
    pub fn move_tenant_already_at_target(
        tenant: impl Into<String>,
        target_db: impl Into<String>,
    ) -> Self {
        let tenant = tenant.into();
        let target_db = target_db.into();
        Self {
            code: ErrorCode::MOVE_TENANT_ALREADY_AT_TARGET,
            message: format!(
                "tenant '{tenant}' is already in database '{target_db}'; \
                 MOVE TENANT is a no-op"
            ),
            details: ErrorDetails::MoveTenantAlreadyAtTarget { tenant, target_db },
            cause: None,
        }
    }
}
