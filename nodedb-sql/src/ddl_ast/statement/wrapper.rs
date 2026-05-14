// SPDX-License-Identifier: Apache-2.0

//! The [`NodedbStatement`] enum — one variant per DDL topic area.
//!
//! Each variant wraps a topic-specific sub-enum defined in
//! `statement/types/`. This file only declares the top-level wrapper.

use super::types::{
    AuthStmt, AutomationStmt, ClusterStmt, CollectionStmt, DatabaseStmt, GraphStmt, MiscStmt,
    PolicyStmt, StreamViewStmt,
};

/// Typed representation of every NodeDB DDL statement.
///
/// Handlers receive a fully-parsed variant instead of raw `&[&str]`
/// parts, eliminating array-index panics and enabling exhaustive
/// match coverage for new DDL commands.
#[derive(Debug, Clone, PartialEq)]
pub enum NodedbStatement {
    Collection(CollectionStmt),
    Automation(AutomationStmt),
    StreamView(StreamViewStmt),
    Policy(PolicyStmt),
    Database(DatabaseStmt),
    Cluster(ClusterStmt),
    Auth(AuthStmt),
    Graph(GraphStmt),
    Misc(MiscStmt),
}
