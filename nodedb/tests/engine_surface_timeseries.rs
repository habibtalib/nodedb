//! Engine surface tests for the Timeseries engine.
//!
//! Covers: ingest, time-range scans, aggregations, retention policy creation,
//! continuous aggregate creation, and WAL durability.

mod common;
use common::pgwire_harness::TestServer;

#[tokio::test]
async fn ingest_and_time_range_scan() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION ts_basic \
         COLUMNS (id TEXT, ts BIGINT TIME_KEY, metric TEXT, value FLOAT) \
         WITH (engine='timeseries')",
    )
    .await
    .unwrap();

    for (i, ts) in [(1u32, 1000u64), (2, 2000), (3, 3000)] {
        srv.exec(&format!(
            "INSERT INTO ts_basic (id, ts, metric, value) VALUES ('p{i}', {ts}, 'cpu', {i}.0)"
        ))
        .await
        .unwrap();
    }

    let rows = srv
        .query_rows("SELECT id FROM ts_basic ORDER BY ts")
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], "p1");
    assert_eq!(rows[2][0], "p3");
}

#[tokio::test]
async fn sum_over_range() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION ts_sum \
         COLUMNS (id TEXT, ts BIGINT TIME_KEY, value FLOAT) \
         WITH (engine='timeseries')",
    )
    .await
    .unwrap();

    for (i, v) in [(1u32, 10.0_f64), (2, 20.0), (3, 30.0), (4, 40.0)] {
        srv.exec(&format!(
            "INSERT INTO ts_sum (id, ts, value) VALUES ('s{i}', {i}000, {v})"
        ))
        .await
        .unwrap();
    }

    let rows = srv
        .query_rows("SELECT SUM(value) FROM ts_sum")
        .await
        .unwrap();
    let total: f64 = rows[0][0].parse().unwrap();
    assert!((total - 100.0).abs() < 0.01);
}

#[tokio::test]
async fn avg_aggregation() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION ts_avg \
         COLUMNS (id TEXT, ts BIGINT TIME_KEY, temp FLOAT) \
         WITH (engine='timeseries')",
    )
    .await
    .unwrap();

    srv.exec("INSERT INTO ts_avg (id, ts, temp) VALUES ('a1', 1000, 20.0)")
        .await
        .unwrap();
    srv.exec("INSERT INTO ts_avg (id, ts, temp) VALUES ('a2', 2000, 30.0)")
        .await
        .unwrap();

    let rows = srv
        .query_rows("SELECT AVG(temp) FROM ts_avg")
        .await
        .unwrap();
    let avg: f64 = rows[0][0].parse().unwrap();
    assert!((avg - 25.0).abs() < 0.01);
}

#[tokio::test]
async fn count_rows() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION ts_cnt \
         COLUMNS (id TEXT, ts BIGINT TIME_KEY, v INT) \
         WITH (engine='timeseries')",
    )
    .await
    .unwrap();

    for i in 0..5u32 {
        srv.exec(&format!(
            "INSERT INTO ts_cnt (id, ts, v) VALUES ('c{i}', {i}000, {i})"
        ))
        .await
        .unwrap();
    }

    let rows = srv.query_rows("SELECT COUNT(*) FROM ts_cnt").await.unwrap();
    assert_eq!(rows[0][0].parse::<u32>().unwrap(), 5);
}

#[tokio::test]
async fn retention_policy_creation() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION ts_ret \
         COLUMNS (id TEXT, ts BIGINT TIME_KEY, v FLOAT) \
         WITH (engine='timeseries')",
    )
    .await
    .unwrap();

    srv.exec("CREATE RETENTION POLICY ret_pol ON ts_ret (RAW RETAIN '7d')")
        .await
        .unwrap();
}

#[tokio::test]
async fn continuous_aggregate_creation() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION ts_cagg_src \
         COLUMNS (id TEXT, ts BIGINT TIME_KEY, value FLOAT) \
         WITH (engine='timeseries')",
    )
    .await
    .unwrap();

    srv.exec(
        "CREATE CONTINUOUS AGGREGATE ts_cagg_view \
         ON ts_cagg_src BUCKET '5m' \
         AGGREGATE SUM(value) AS total_value",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn wal_restart_durability() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION ts_wal \
         COLUMNS (id TEXT, ts BIGINT TIME_KEY, val FLOAT) \
         WITH (engine='timeseries')",
    )
    .await
    .unwrap();
    srv.exec("INSERT INTO ts_wal (id, ts, val) VALUES ('w1', 9999, 3.14)")
        .await
        .unwrap();

    let (srv, dir) = srv.take_dir();
    srv.graceful_shutdown().await;

    let (srv2, _dir) = TestServer::open_on_path(dir).await;
    let rows = srv2
        .query_rows("SELECT val FROM ts_wal WHERE id = 'w1'")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let v: f64 = rows[0][0].parse().unwrap();
    #[allow(clippy::approx_constant)]
    let expected = 3.14_f64;
    assert!((v - expected).abs() < 0.01);
}
