// SPDX-License-Identifier: Apache-2.0

//! Parse cluster-admin: SHOW CLUSTER/NODES/RAFT/MIGRATIONS/RANGES/ROUTING,
//! REMOVE NODE, REBALANCE, ALTER RAFT GROUP.

use crate::ddl_ast::statement::{ClusterStmt, NodedbStatement};
use crate::error::SqlError;

pub(super) fn try_parse(
    upper: &str,
    parts: &[&str],
    _trimmed: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    (|| -> Option<NodedbStatement> {
        if upper.starts_with("SHOW CLUSTER") {
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowCluster));
        }
        if upper.starts_with("SHOW MIGRATIONS") {
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowMigrations));
        }
        if upper.starts_with("SHOW RANGES") {
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowRanges));
        }
        if upper.starts_with("SHOW ROUTING") {
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowRouting));
        }
        if upper.starts_with("SHOW SCHEMA VERSION") {
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowSchemaVersion));
        }
        if upper.starts_with("SHOW PEER HEALTH") {
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowPeerHealth));
        }
        if upper.starts_with("REBALANCE") {
            return Some(NodedbStatement::Cluster(ClusterStmt::Rebalance));
        }
        if upper.starts_with("SHOW RAFT GROUP ") {
            let id = parts.get(3)?.to_string();
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowRaftGroup {
                group_id: id,
            }));
        }
        if upper.starts_with("SHOW RAFT GROUPS") || upper.starts_with("SHOW RAFT") {
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowRaftGroups));
        }
        if upper.starts_with("ALTER RAFT GROUP ") {
            // ALTER RAFT GROUP <group_id> ADD|REMOVE NODE <node_id>
            let group_id = parts.get(3).map(|s| s.to_string()).unwrap_or_default();
            let action = parts.get(4).map(|s| s.to_uppercase()).unwrap_or_default();
            let node_id = parts.get(6).map(|s| s.to_string()).unwrap_or_default();
            return Some(NodedbStatement::Cluster(ClusterStmt::AlterRaftGroup {
                group_id,
                action,
                node_id,
            }));
        }
        if upper.starts_with("REMOVE NODE ") {
            let id = parts.get(2)?.to_string();
            return Some(NodedbStatement::Cluster(ClusterStmt::RemoveNode {
                node_id: id,
            }));
        }
        if upper.starts_with("SHOW NODE ") {
            let id = parts.get(2)?.to_string();
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowNode {
                node_id: id,
            }));
        }
        if upper.starts_with("SHOW NODES") {
            return Some(NodedbStatement::Cluster(ClusterStmt::ShowNodes));
        }
        None
    })()
    .map(Ok)
}
