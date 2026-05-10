// SPDX-License-Identifier: BUSL-1.1

//! Mirror database operations.
//!
//! Lifecycle:
//! 1. `MIRROR DATABASE <local> FROM <cluster>.<src>` ([`create`]) creates a
//!    `Bootstrapping` replica descriptor and triggers the cross-cluster
//!    observer link.
//! 2. The observer stream delivers Raft DDL entries from the source; each is
//!    applied via [`ddl_apply::apply_mirror_ddl_entry`], which atomically
//!    updates the mirror collection map and the lag record in one catalog
//!    transaction (LSN-idempotent).
//! 3. Reads are gated by [`read::check_mirror_read_consistency`] using the
//!    session's `ReadConsistency` (Strong rejects, BoundedStaleness compares
//!    lag, Eventual passes); writes are rejected with `MIRROR_READ_ONLY`
//!    until promotion.
//! 4. `ALTER DATABASE <name> PROMOTE` ([`promote`]) flips the descriptor to
//!    a writable primary (`MirrorStatus::Promoted` + `DatabaseStatus::Active`).
//! 5. `SHOW DATABASE MIRROR STATUS` ([`show`]) exposes lifecycle state and
//!    lag metrics for operators.

pub mod create;
pub mod ddl_apply;
pub mod promote;
pub mod read;
pub mod show;

pub use create::handle_mirror_database;
pub use ddl_apply::{MirrorDdlKind, apply_mirror_ddl_entry};
pub use promote::handle_promote_database;
pub use read::{MirrorReadOutcome, check_mirror_read_consistency};
pub use show::handle_show_database_mirror_status;
