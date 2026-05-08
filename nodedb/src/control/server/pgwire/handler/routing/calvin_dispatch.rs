// SPDX-License-Identifier: BUSL-1.1

//! Calvin multi-shard distributed dispatch.
//!
//! Handles the strict multi-shard path via the Calvin sequencer, including
//! the OLLP-dependent-predicate variant that runs an optimistic pre-execution
//! scan before submitting the transaction.

use pgwire::api::results::Response;
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};

use crate::control::planner::calvin::preexec::run_preexec_scan;
use crate::control::planner::calvin::{
    DispatchOutcome, build_dependent_tx_class, dispatch_calvin_or_fast, dispatch_dependent_read,
    is_dependent_predicate, predicate_class,
};
use crate::control::planner::physical::PhysicalTask;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::types::TenantId;

use super::super::super::types::error_to_sqlstate;
use super::super::core::NodeDbPgHandler;
use super::ollp_helpers::{extract_bulk_predicate_info, inject_ollp_surrogates};
use super::planning::calvin_execution_response;

impl NodeDbPgHandler {
    /// Drive Calvin strict multi-shard dispatch for the given task set.
    ///
    /// Returns the response vec on success (one tag per task). The caller
    /// should return this immediately — Calvin tasks do not go through the
    /// per-task dispatch loop.
    pub(super) async fn dispatch_calvin_multishard(
        &self,
        tasks: Vec<PhysicalTask>,
        tenant_id: TenantId,
        _identity: &AuthenticatedIdentity,
        addr: &std::net::SocketAddr,
    ) -> PgWireResult<Vec<Response>> {
        let cross_shard_mode = self.sessions.cross_shard_txn_mode(addr);
        let tx_state = self.sessions.transaction_state(addr);

        let inbox = self.state.sequencer_inbox.get();
        let orchestrator = self.state.ollp_orchestrator.get();
        let registry = self.state.calvin_completion_registry.get().ok_or_else(|| {
            let (severity, code, message) = error_to_sqlstate(&crate::Error::SequencerUnavailable);
            PgWireError::UserError(Box::new(ErrorInfo::new(
                severity.to_owned(),
                code.to_owned(),
                message,
            )))
        })?;

        let dependent_task = tasks.iter().find(|t| is_dependent_predicate(&t.plan));

        let inbox_seq = if let Some(dep_task) = dependent_task {
            // OLLP path: run optimistic pre-execution scan, then submit
            // via dispatch_dependent_read with the predicted surrogates
            // embedded in the plan.
            let orc = orchestrator.ok_or_else(|| {
                let (severity, code, message) =
                    error_to_sqlstate(&crate::Error::SequencerUnavailable);
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    severity.to_owned(),
                    code.to_owned(),
                    message,
                )))
            })?;
            let inbox = inbox.ok_or_else(|| {
                let (severity, code, message) =
                    error_to_sqlstate(&crate::Error::SequencerUnavailable);
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    severity.to_owned(),
                    code.to_owned(),
                    message,
                )))
            })?;

            let (dep_collection, dep_filter_bytes) = extract_bulk_predicate_info(&dep_task.plan);
            let pred_class = predicate_class(&dep_collection, &dep_collection);

            let predicted = run_preexec_scan(
                &self.state,
                tenant_id,
                dep_task.database_id,
                &dep_collection,
                dep_filter_bytes.clone(),
            )
            .await
            .map_err(|e| {
                let (severity, code, message) = error_to_sqlstate(&e);
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    severity.to_owned(),
                    code.to_owned(),
                    message,
                )))
            })?;

            let tasks_snapshot = tasks.clone();
            let collection_clone = dep_collection.clone();
            let predicted_clone = predicted.clone();

            dispatch_dependent_read(
                orc,
                inbox,
                pred_class,
                tenant_id,
                || {
                    let modified_tasks: Vec<PhysicalTask> = tasks_snapshot
                        .iter()
                        .map(|t| {
                            let mut t = t.clone();
                            inject_ollp_surrogates(&mut t.plan, predicted_clone.clone());
                            t
                        })
                        .collect();

                    build_dependent_tx_class(
                        &modified_tasks,
                        tenant_id,
                        &collection_clone,
                        &predicted_clone,
                    )
                },
                orc.ollp_max_retries(),
            )
            .await
            .map_err(|e| {
                let (severity, code, message) = error_to_sqlstate(&e);
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    severity.to_owned(),
                    code.to_owned(),
                    message,
                )))
            })?
        } else {
            // Static Calvin path: all write keys are statically known.
            let dispatch = dispatch_calvin_or_fast(
                &tasks,
                cross_shard_mode,
                tx_state,
                inbox,
                orchestrator,
                tenant_id,
            )
            .await
            .map_err(|e| {
                let (severity, code, message) = error_to_sqlstate(&e);
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    severity.to_owned(),
                    code.to_owned(),
                    message,
                )))
            })?;

            match dispatch {
                DispatchOutcome::CalvinStatic { inbox_seq }
                | DispatchOutcome::CalvinDependent { inbox_seq } => inbox_seq,
                DispatchOutcome::SingleShard | DispatchOutcome::BestEffortNonAtomic => {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".to_owned(),
                        "XX000".to_owned(),
                        "unexpected non-Calvin dispatch outcome for strict \
                         multi-shard query"
                            .to_owned(),
                    ))));
                }
            }
        };

        let assignment_rx = registry.register_submission(inbox_seq);
        let timeout =
            std::time::Duration::from_secs(self.state.tuning.network.default_deadline_secs);
        let (epoch, position, _participants) = tokio::time::timeout(timeout, assignment_rx)
            .await
            .map_err(|_| {
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".to_owned(),
                    "57014".to_owned(),
                    "timed out waiting for Calvin sequencer assignment".to_owned(),
                )))
            })?
            .map_err(|_| {
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".to_owned(),
                    "XX000".to_owned(),
                    "Calvin sequencer assignment channel closed".to_owned(),
                )))
            })?;

        let completion_rx =
            registry.register_completion(nodedb_cluster::calvin::TxnId::new(epoch, position));
        tokio::time::timeout(timeout, completion_rx)
            .await
            .map_err(|_| {
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".to_owned(),
                    "57014".to_owned(),
                    "timed out waiting for Calvin transaction completion".to_owned(),
                )))
            })?
            .map_err(|_| {
                PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".to_owned(),
                    "XX000".to_owned(),
                    "Calvin completion channel closed".to_owned(),
                )))
            })?;

        // Emit one CommandComplete tag per accumulated task.
        let mut calvin_responses: Vec<Response> = Vec::with_capacity(tasks.len());
        for task in &tasks {
            calvin_responses.push(calvin_execution_response(task));
        }
        Ok(calvin_responses)
    }
}
