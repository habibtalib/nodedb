//! Basic cluster boot and single-node create tests.

use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_node_cluster_boots() {
    // Smallest possible smoke test: one node in cluster mode.
    let node = crate::common::cluster_harness::TestClusterNode::spawn(1, vec![])
        .await
        .expect("single-node cluster spawn");
    assert_eq!(node.topology_size(), 1);
    node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn single_node_cluster_create_collection() {
    // Isolates the pgwire handler → propose_metadata_and_wait path
    // on a single-node cluster so cluster-formation noise (elections,
    // joining learners) is out of the picture.
    let node = crate::common::cluster_harness::TestClusterNode::spawn(1, vec![])
        .await
        .expect("spawn");
    // Give the raft tick a moment to process any startup entries.
    tokio::time::sleep(Duration::from_millis(200)).await;
    node.exec("CREATE COLLECTION widgets")
        .await
        .expect("create widgets");
    assert_eq!(node.cached_collection_count(), 1);
    node.shutdown().await;
}
