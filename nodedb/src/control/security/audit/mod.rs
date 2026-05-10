// SPDX-License-Identifier: BUSL-1.1

//! Immutable audit log for security-relevant events.
//!
//! The log is organized into focused submodules:
//!
//! - [`auth`] — authenticated-identity context attached to every entry.
//! - [`level`] — the recorded-event severity filter.
//! - [`event`] — the `AuditEvent` enum + level/routing rules.
//! - [`entry`] — the durable `AuditEntry` struct + hash-chain helper.
//! - [`ddl_detail`] — structured detail body for `AuditEvent::DdlChange`.
//! - [`undrop_detail`] — structured detail body for `UNDROP COLLECTION`.
//! - [`log`] — the in-memory append-only `AuditLog` itself.

pub mod auth;
pub mod ddl_detail;
pub mod emitter;
pub mod entry;
pub mod event;
pub mod level;
pub mod log;
pub mod undrop_detail;

pub use auth::AuditAuth;
pub use ddl_detail::DdlAuditDetail;
pub use emitter::{ArcAuditEmitter, AuditEmitContext, AuditEmitter, NoopAuditEmitter};
pub use entry::AuditEntry;
pub use event::AuditEvent;
pub use level::AuditLevel;
pub use log::AuditLog;
pub use undrop_detail::{UndropAuditDetail, UndropStage};
