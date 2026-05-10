// SPDX-License-Identifier: Apache-2.0

//! Parser for database-level DDL: CREATE / DROP / ALTER / USE / CLONE /
//! MIRROR / MOVE TENANT / BACKUP / RESTORE / SHOW DATABASE(S).

mod alter;
mod backup_restore;
mod clone;
mod create;
mod dispatch;
mod drop_db;
mod mirror;
mod move_tenant;
mod quota_spec;
mod show_extras;
mod use_db;
mod with_options;

pub use dispatch::try_parse;
pub use quota_spec::parse_quota_spec;
