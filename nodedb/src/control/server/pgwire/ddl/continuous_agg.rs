//! DDL handlers for continuous aggregates.
//!
//! - `CREATE CONTINUOUS AGGREGATE <name> ON <source> BUCKET '5m' AGGREGATE sum(col) AS alias [, ...] [GROUP BY col, ...] [WITH (...)]`
//! - `DROP CONTINUOUS AGGREGATE <name>`
//! - `SHOW CONTINUOUS AGGREGATES [FOR <source>]`

use std::sync::Arc;
use std::time::Duration;

use futures::stream;
use pgwire::api::results::{DataRowEncoder, QueryResponse, Response};
use pgwire::error::PgWireResult;

use crate::bridge::envelope::PhysicalPlan;
use crate::bridge::physical_plan::MetaOp;
use crate::control::security::identity::AuthenticatedIdentity;
use crate::control::state::SharedState;
use crate::engine::timeseries::continuous_agg::{AggregateInfo, ContinuousAggregateDef};

use super::super::types::{int8_field, sqlstate_error, text_field};

#[path = "continuous_agg_parse.rs"]
mod continuous_agg_parse;
use continuous_agg_parse::{extract_with_options, parse_create_sql};

/// CREATE CONTINUOUS AGGREGATE <name> ON <source> BUCKET '<interval>'
///   AGGREGATE <func>(col) [AS alias], ...
///   [GROUP BY col, ...]
///   [WITH (refresh_policy = 'on_flush', retention = '7d')]
/// Parsed `CREATE CONTINUOUS AGGREGATE` request.
///
/// `aggregate_exprs_raw` is the raw text after AGGREGATE keyword.
/// `with_clause_raw` is the raw inner text of the trailing WITH(...), or empty.
#[derive(Clone, Copy)]
pub struct CreateContinuousAggregateRequest<'a> {
    pub name: &'a str,
    pub source: &'a str,
    pub bucket_raw: &'a str,
    pub aggregate_exprs_raw: &'a str,
    pub group_by: &'a [String],
    pub with_clause_raw: &'a str,
}

/// Handle `CREATE CONTINUOUS AGGREGATE`.
pub async fn create_continuous_aggregate(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    req: &CreateContinuousAggregateRequest<'_>,
) -> PgWireResult<Vec<Response>> {
    let CreateContinuousAggregateRequest {
        name,
        source,
        bucket_raw,
        aggregate_exprs_raw,
        group_by,
        with_clause_raw,
    } = *req;
    // Reconstruct minimal SQL for parse_create_sql reuse.
    // This avoids duplicating the complex AggregateExpr parsing logic.
    let reconstructed = format!(
        "CREATE CONTINUOUS AGGREGATE {name} ON {source} BUCKET '{bucket_raw}' AGGREGATE {aggregate_exprs_raw}"
    );
    let def_from_parts = parse_create_sql(&reconstructed)?;

    // Apply group_by and with_clause_raw overrides.
    let (refresh_policy, retention_period_ms) = if with_clause_raw.is_empty() {
        (
            def_from_parts.refresh_policy,
            def_from_parts.retention_period_ms,
        )
    } else {
        let fake_with_sql = format!("dummy WITH ({with_clause_raw})");
        let (rp, ret) = extract_with_options(&fake_with_sql.to_uppercase(), &fake_with_sql);
        (rp, ret)
    };

    let def = ContinuousAggregateDef {
        name: def_from_parts.name,
        source: def_from_parts.source,
        bucket_interval: def_from_parts.bucket_interval,
        bucket_interval_ms: def_from_parts.bucket_interval_ms,
        group_by: group_by.to_vec(),
        aggregates: def_from_parts.aggregates,
        refresh_policy,
        retention_period_ms,
        stale: false,
    };

    // Validate source collection exists and is timeseries.
    let tenant_id = identity.tenant_id;
    if let Some(catalog) = state.credentials.catalog() {
        match catalog.get_collection(tenant_id.as_u64(), &def.source) {
            Ok(Some(coll)) if coll.collection_type.is_timeseries() => {}
            Ok(Some(_)) => {
                return Err(sqlstate_error(
                    "42809",
                    &format!("'{}' is not a timeseries collection", def.source),
                ));
            }
            _ => {
                return Err(sqlstate_error(
                    "42P01",
                    &format!("collection '{}' does not exist", def.source),
                ));
            }
        }
    }

    // Dispatch registration to Data Plane.
    let plan = PhysicalPlan::Meta(MetaOp::RegisterContinuousAggregate { def: def.clone() });
    super::sync_dispatch::dispatch_async(
        state,
        tenant_id,
        &def.source,
        plan,
        Duration::from_secs(5),
    )
    .await
    .map_err(|e| sqlstate_error("XX000", &format!("dispatch failed: {e}")))?;

    tracing::info!(
        name = def.name,
        source = def.source,
        interval = def.bucket_interval,
        tenant = tenant_id.as_u64(),
        "continuous aggregate created"
    );

    Ok(vec![Response::Execution(pgwire::api::results::Tag::new(
        "CREATE CONTINUOUS AGGREGATE",
    ))])
}

/// DROP CONTINUOUS AGGREGATE <name>
pub async fn drop_continuous_aggregate(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    if parts.len() < 4 {
        return Err(sqlstate_error(
            "42601",
            "syntax: DROP CONTINUOUS AGGREGATE <name>",
        ));
    }

    let name = parts[3].to_lowercase();
    let tenant_id = identity.tenant_id;

    let plan = PhysicalPlan::Meta(MetaOp::UnregisterContinuousAggregate { name: name.clone() });

    super::sync_dispatch::dispatch_async(state, tenant_id, &name, plan, Duration::from_secs(5))
        .await
        .map_err(|e| sqlstate_error("XX000", &format!("dispatch failed: {e}")))?;

    tracing::info!(name, "continuous aggregate dropped");

    Ok(vec![Response::Execution(pgwire::api::results::Tag::new(
        "DROP CONTINUOUS AGGREGATE",
    ))])
}

/// SHOW CONTINUOUS AGGREGATES [FOR <source>]
///
/// Dispatches to the Data Plane to query `manager.list_aggregates()` and
/// returns the results as pgwire rows.
pub async fn show_continuous_aggregates(
    state: &SharedState,
    identity: &AuthenticatedIdentity,
    parts: &[&str],
) -> PgWireResult<Vec<Response>> {
    let source_filter = if parts.len() >= 5 && parts[3].to_uppercase() == "FOR" {
        Some(parts[4].to_lowercase())
    } else {
        None
    };

    let tenant_id = identity.tenant_id;

    // Dispatch to Data Plane to get aggregate list.
    // Use "__system" as collection for vShard routing (meta operation).
    let plan = PhysicalPlan::Meta(MetaOp::ListContinuousAggregates);
    let payload = super::sync_dispatch::dispatch_async(
        state,
        tenant_id,
        "__system",
        plan,
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_default();

    let infos: Vec<AggregateInfo> = sonic_rs::from_slice(&payload).unwrap_or_default();

    let schema = Arc::new(vec![
        text_field("name"),
        text_field("source"),
        text_field("bucket_interval"),
        text_field("refresh_policy"),
        int8_field("watermark_ts"),
        int8_field("rows_aggregated"),
        int8_field("materialized_buckets"),
        text_field("stale"),
    ]);

    let mut rows = Vec::new();
    for info in &infos {
        // Apply source filter if specified.
        if let Some(ref filter) = source_filter
            && info.source != *filter
        {
            continue;
        }

        let mut encoder = DataRowEncoder::new(schema.clone());
        encoder
            .encode_field(&info.name)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        encoder
            .encode_field(&info.source)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        encoder
            .encode_field(&info.bucket_interval)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        encoder
            .encode_field(&format!("{:?}", info.refresh_policy))
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        encoder
            .encode_field(&info.watermark_ts)
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        encoder
            .encode_field(&(info.rows_aggregated as i64))
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        encoder
            .encode_field(&(info.materialized_buckets as i64))
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        encoder
            .encode_field(&info.stale.to_string())
            .map_err(|e| sqlstate_error("XX000", &e.to_string()))?;
        rows.push(Ok(encoder.take_row()));
    }

    Ok(vec![Response::Query(QueryResponse::new(
        schema,
        stream::iter(rows),
    ))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::timeseries::continuous_agg::{AggFunction, RefreshPolicy};

    #[test]
    fn parse_create_basic() {
        let sql = "CREATE CONTINUOUS AGGREGATE metrics_1m ON metrics \
                    BUCKET '1m' \
                    AGGREGATE sum(value) AS value_sum, count(*) AS row_count";
        let def = parse_create_sql(sql).expect("parse_create_sql failed");
        assert_eq!(def.name, "metrics_1m");
        assert_eq!(def.source, "metrics");
        assert_eq!(def.bucket_interval, "1m");
        assert_eq!(def.bucket_interval_ms, 60_000);
        assert_eq!(def.aggregates.len(), 2);
        assert_eq!(def.aggregates[0].output_column, "value_sum");
        assert_eq!(def.aggregates[1].output_column, "row_count");
        assert!(matches!(def.aggregates[0].function, AggFunction::Sum));
        assert!(matches!(def.aggregates[1].function, AggFunction::Count));
    }

    #[test]
    fn parse_create_with_group_by_and_options() {
        let sql = "CREATE CONTINUOUS AGGREGATE cpu_5m ON cpu_metrics \
                    BUCKET '5m' \
                    AGGREGATE avg(cpu) AS cpu_avg, max(cpu) AS cpu_max \
                    GROUP BY host, region \
                    WITH (refresh_policy = 'on_flush', retention = '7d')";
        let def = parse_create_sql(sql).unwrap();
        assert_eq!(def.name, "cpu_5m");
        assert_eq!(def.source, "cpu_metrics");
        assert_eq!(def.bucket_interval_ms, 300_000);
        assert_eq!(def.group_by, vec!["host", "region"]);
        assert_eq!(def.refresh_policy, RefreshPolicy::OnFlush);
        assert_eq!(def.retention_period_ms, 604_800_000); // 7d
    }

    #[test]
    fn parse_create_auto_alias() {
        let sql = "CREATE CONTINUOUS AGGREGATE m1 ON metrics \
                    BUCKET '1h' \
                    AGGREGATE min(value), max(value)";
        let def = parse_create_sql(sql).unwrap();
        assert_eq!(def.aggregates[0].output_column, "min_value");
        assert_eq!(def.aggregates[1].output_column, "max_value");
    }

    #[test]
    fn parse_create_manual_refresh() {
        let sql = "CREATE CONTINUOUS AGGREGATE m1 ON metrics \
                    BUCKET '1d' \
                    AGGREGATE sum(val) \
                    WITH (refresh_policy = 'manual')";
        let def = parse_create_sql(sql).unwrap();
        assert_eq!(def.refresh_policy, RefreshPolicy::Manual);
    }

    #[test]
    fn parse_create_periodic_refresh() {
        let sql = "CREATE CONTINUOUS AGGREGATE m1 ON metrics \
                    BUCKET '1h' \
                    AGGREGATE count(*) \
                    WITH (refresh = '5m')";
        let def = parse_create_sql(sql).unwrap();
        assert_eq!(def.refresh_policy, RefreshPolicy::Periodic(300_000));
    }

    #[test]
    fn parse_missing_bucket_errors() {
        let sql = "CREATE CONTINUOUS AGGREGATE m1 ON metrics AGGREGATE sum(val)";
        assert!(parse_create_sql(sql).is_err());
    }
}
