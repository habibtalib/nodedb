// SPDX-License-Identifier: BUSL-1.1

//! Authentication object replication tests: users, roles, API keys.

use std::time::Duration;

use crate::common::cluster_harness::{TestCluster, wait_for};

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn user_create_visible_on_every_node() {
    let cluster = TestCluster::spawn_three().await.expect("3-node cluster");

    cluster
        .exec_ddl_on_any_leader("CREATE USER alice WITH PASSWORD 'sekret123' ROLE read_write")
        .await
        .expect("create user");

    wait_for(
        "all 3 nodes see the replicated user in credentials",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || cluster.nodes.iter().all(|n| n.has_active_user("alice")),
    )
    .await;

    cluster
        .exec_ddl_on_any_leader("DROP USER alice")
        .await
        .expect("drop user");

    wait_for(
        "all 3 nodes see alice as deactivated",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || cluster.nodes.iter().all(|n| !n.has_active_user("alice")),
    )
    .await;

    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn role_create_visible_on_every_node() {
    let cluster = TestCluster::spawn_three().await.expect("3-node cluster");

    cluster
        .exec_ddl_on_any_leader("CREATE ROLE data_analyst")
        .await
        .expect("create role");

    wait_for(
        "all 3 nodes see the replicated role",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || cluster.nodes.iter().all(|n| n.has_role("data_analyst")),
    )
    .await;

    cluster
        .exec_ddl_on_any_leader("DROP ROLE data_analyst")
        .await
        .expect("drop role");

    wait_for(
        "all 3 nodes no longer see the role",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || cluster.nodes.iter().all(|n| !n.has_role("data_analyst")),
    )
    .await;

    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn alter_user_role_replicates() {
    let cluster = TestCluster::spawn_three().await.expect("3-node cluster");

    cluster
        .exec_ddl_on_any_leader("CREATE USER bob WITH PASSWORD 'initial-pass' ROLE read_only")
        .await
        .expect("create user");

    wait_for(
        "all 3 nodes see bob with read_only role",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || {
            cluster
                .nodes
                .iter()
                .all(|n| n.user_has_role("bob", "read_only"))
        },
    )
    .await;

    cluster
        .exec_ddl_on_any_leader("ALTER USER bob SET ROLE read_write")
        .await
        .expect("alter user set role");

    wait_for(
        "all 3 nodes see bob with read_write role",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || {
            cluster
                .nodes
                .iter()
                .all(|n| n.user_has_role("bob", "read_write"))
        },
    )
    .await;

    cluster.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn api_key_create_and_revoke_replicates() {
    let cluster = TestCluster::spawn_three().await.expect("3-node cluster");

    cluster
        .exec_ddl_on_any_leader("CREATE USER charlie WITH PASSWORD 'pw-charlie-1'")
        .await
        .expect("create user");

    wait_for(
        "all 3 nodes see charlie",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || cluster.nodes.iter().all(|n| n.has_active_user("charlie")),
    )
    .await;

    let all_nodes_have_key = |cluster: &TestCluster| -> bool {
        cluster
            .nodes
            .iter()
            .all(|n| !n.shared.api_keys.list_keys_for_user("charlie").is_empty())
    };

    assert!(!all_nodes_have_key(&cluster));

    cluster
        .exec_ddl_on_any_leader("CREATE API KEY FOR charlie")
        .await
        .expect("create api key");

    wait_for(
        "all 3 nodes see a replicated API key for charlie",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || all_nodes_have_key(&cluster),
    )
    .await;

    // Pick the key_id from any node's cache and revoke it.
    let key_id = cluster.nodes[0]
        .shared
        .api_keys
        .list_keys_for_user("charlie")
        .first()
        .map(|k| k.key_id.clone())
        .expect("key replicated");

    cluster
        .exec_ddl_on_any_leader(&format!("REVOKE API KEY {key_id}"))
        .await
        .expect("revoke api key");

    wait_for(
        "all 3 nodes see the key as revoked",
        Duration::from_secs(10),
        Duration::from_millis(50),
        || cluster.nodes.iter().all(|n| !n.has_active_api_key(&key_id)),
    )
    .await;

    cluster.shutdown().await;
}
