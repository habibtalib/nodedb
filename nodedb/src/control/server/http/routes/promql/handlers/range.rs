//! GET/POST `/obsv/api/v1/query_range` — range PromQL query.

use axum::extract::{Query, State};
use axum::response::IntoResponse;

use crate::control::promql;
use crate::control::server::http::auth::{AppState, ResolvedIdentity};

use crate::control::server::http::routes::promql::RangeQueryParams;
use crate::control::server::http::routes::promql::helpers::{
    fetch_series_for_query, parse_step, prom_error, prom_success,
};

pub async fn range_query(
    _identity: ResolvedIdentity,
    State(state): State<AppState>,
    Query(params): Query<RangeQueryParams>,
) -> impl IntoResponse {
    let start_ms = (params.start * 1000.0) as i64;
    let end_ms = (params.end * 1000.0) as i64;
    let step_ms = parse_step(&params.step).unwrap_or(15_000);

    if step_ms <= 0 {
        return prom_error("bad_data", "step must be positive");
    }
    if end_ms < start_ms {
        return prom_error("bad_data", "end must be >= start");
    }

    let tokens = match promql::lexer::tokenize(&params.query) {
        Ok(t) => t,
        Err(e) => return prom_error("bad_data", &e.to_string()),
    };
    let expr = match promql::parse(&tokens) {
        Ok(e) => e,
        Err(e) => return prom_error("bad_data", &e.to_string()),
    };

    let series = fetch_series_for_query(
        &state,
        start_ms - promql::types::DEFAULT_LOOKBACK_MS,
        end_ms,
    )
    .await;

    let ctx = promql::EvalContext {
        series,
        timestamp_ms: start_ms,
        lookback_ms: promql::types::DEFAULT_LOOKBACK_MS,
    };

    match promql::evaluate_range(&ctx, &expr, start_ms, end_ms, step_ms) {
        Ok(value) => prom_success(value),
        Err(e) => prom_error("execution", &e.to_string()),
    }
}
