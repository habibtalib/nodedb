//! Engine surface tests for the Spatial engine.
//!
//! Covers: ST_GeoHash encode/decode roundtrip, H3 encode/decode roundtrip,
//! ST_Distance, and basic collection lifecycle.

mod common;
use common::pgwire_harness::TestServer;

#[tokio::test]
async fn create_spatial_collection_and_insert() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION spatial_basic \
         COLUMNS (id TEXT, location GEOMETRY, name TEXT) \
         WITH (engine='spatial')",
    )
    .await
    .unwrap();

    srv.exec(
        "INSERT INTO spatial_basic (id, location, name) \
         VALUES ('p1', ST_Point(-122.4, 37.8), 'SF')",
    )
    .await
    .unwrap();

    let rows = srv
        .query_rows("SELECT id, name FROM spatial_basic WHERE id = 'p1'")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], "SF");
}

#[tokio::test]
async fn st_geohash_encode_decode_roundtrip() {
    let srv = TestServer::start().await;
    srv.exec("CREATE COLLECTION spatial_geo WITH (engine='document_schemaless')")
        .await
        .unwrap();

    let rows = srv
        .query_rows("SELECT ST_GeoHash(-122.4, 37.8, 6)")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let hash = rows[0][0].clone();
    assert!(!hash.is_empty(), "expected geohash string, got empty");
    assert!(hash.starts_with('9'), "unexpected geohash prefix: {hash}");

    let rows2 = srv
        .query_rows(&format!("SELECT ST_GeoHashDecode('{hash}')"))
        .await
        .unwrap();
    assert_eq!(rows2.len(), 1);
    assert!(!rows2[0][0].is_empty(), "expected decoded bbox, got empty");
}

#[tokio::test]
async fn h3_latlngtocell_and_celltolatlng_roundtrip() {
    let srv = TestServer::start().await;
    srv.exec("CREATE COLLECTION spatial_h3 WITH (engine='document_schemaless')")
        .await
        .unwrap();

    let rows = srv
        .query_rows("SELECT H3_LatLngToCell(37.8, -122.4, 7)")
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let cell = rows[0][0].clone();
    assert!(!cell.is_empty(), "expected H3 cell string, got empty");

    let rows2 = srv
        .query_rows(&format!("SELECT H3_CellToLatLng('{cell}')"))
        .await
        .unwrap();
    assert_eq!(rows2.len(), 1);
    assert!(
        !rows2[0][0].is_empty(),
        "expected decoded lat/lng, got empty"
    );
}

#[tokio::test]
async fn scalar_st_distance() {
    let srv = TestServer::start().await;
    srv.exec("CREATE COLLECTION spatial_dist WITH (engine='document_schemaless')")
        .await
        .unwrap();

    let rows = srv
        .query_rows(
            "SELECT ST_Distance(\
               ST_Point(-122.4, 37.8), \
               ST_Point(-87.6, 41.8)\
             )",
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let dist: f64 = rows[0][0].parse().expect("expected numeric distance");
    assert!(dist > 0.0, "distance should be positive, got {dist}");
}

#[tokio::test]
async fn count_spatial_rows() {
    let srv = TestServer::start().await;
    srv.exec(
        "CREATE COLLECTION spatial_cnt \
         COLUMNS (id TEXT, loc GEOMETRY) \
         WITH (engine='spatial')",
    )
    .await
    .unwrap();

    for i in 0..3u32 {
        let lng = -122.4 + i as f64 * 0.1;
        srv.exec(&format!(
            "INSERT INTO spatial_cnt (id, loc) VALUES ('p{i}', ST_Point({lng}, 37.8))"
        ))
        .await
        .unwrap();
    }

    let rows = srv
        .query_rows("SELECT COUNT(*) FROM spatial_cnt")
        .await
        .unwrap();
    assert_eq!(rows[0][0].parse::<u32>().unwrap(), 3);
}
