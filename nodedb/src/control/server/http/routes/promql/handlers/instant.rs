//! GET/POST `/obsv/api/v1/query` — instant PromQL query.

use axum::extract::{Query, State};
use axum::response::IntoResponse;

use crate::control::promql;
use crate::control::server::http::auth::{AppState, ResolvedIdentity};

use crate::control::server::http::routes::promql::InstantQueryParams;
use crate::control::server::http::routes::promql::helpers::{
    fetch_series_for_query, prom_error, prom_success,
};

pub async fn instant_query(
    _identity: ResolvedIdentity,
    State(state): State<AppState>,
    Query(params): Query<InstantQueryParams>,
) -> impl IntoResponse {
    let ts_ms = params.time.map(|t| (t * 1000.0) as i64).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    });

    let tokens = match promql::lexer::tokenize(&params.query) {
        Ok(t) => t,
        Err(e) => return prom_error("bad_data", &e.to_string()),
    };
    let expr = match promql::parse(&tokens) {
        Ok(e) => e,
        Err(e) => return prom_error("bad_data", &e.to_string()),
    };

    let series =
        fetch_series_for_query(&state, ts_ms - promql::types::DEFAULT_LOOKBACK_MS, ts_ms).await;

    let ctx = promql::EvalContext {
        series,
        timestamp_ms: ts_ms,
        lookback_ms: promql::types::DEFAULT_LOOKBACK_MS,
    };

    match promql::evaluate_instant(&ctx, &expr) {
        Ok(value) => prom_success(value),
        Err(e) => prom_error("execution", &e.to_string()),
    }
}
