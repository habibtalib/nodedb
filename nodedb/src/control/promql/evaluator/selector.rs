//! Vector and matrix selector evaluation.

use super::super::ast::*;
use super::super::error::PromqlError;
use super::super::label::matches_all;
use super::super::types::*;
use super::EvalContext;

pub fn eval_vector_selector(
    ctx: &EvalContext,
    name: Option<&str>,
    matchers: &[super::super::label::LabelMatcher],
    offset: Option<Duration>,
) -> Result<Value, PromqlError> {
    let eval_ts = ctx.timestamp_ms - offset.map_or(0, |d| d.ms());
    let min_ts = eval_ts - ctx.lookback_ms;

    let mut result = Vec::new();
    for series in &ctx.series {
        if let Some(n) = name
            && series.metric_name() != n
        {
            continue;
        }
        if !matches_all(matchers, &series.labels) {
            continue;
        }

        let sample = series
            .samples
            .iter()
            .rev()
            .find(|s| s.timestamp_ms >= min_ts && s.timestamp_ms <= eval_ts);

        if let Some(s) = sample {
            result.push(InstantSample {
                labels: series.labels.clone(),
                value: s.value,
                timestamp_ms: eval_ts,
            });
        }
    }
    Ok(Value::Vector(result))
}

pub fn eval_matrix_selector(
    ctx: &EvalContext,
    selector: &Expr,
    range: Duration,
) -> Result<Value, PromqlError> {
    let Expr::VectorSelector {
        name,
        matchers,
        offset,
    } = selector
    else {
        return Err(PromqlError::Selector {
            detail: "matrix selector requires vector selector".to_string(),
        });
    };

    let eval_ts = ctx.timestamp_ms - offset.map_or(0, |d| d.ms());
    let range_start = eval_ts - range.ms();

    let mut result = Vec::new();
    for series in &ctx.series {
        if let Some(n) = name
            && series.metric_name() != n
        {
            continue;
        }
        if !matches_all(matchers, &series.labels) {
            continue;
        }

        let window: Vec<Sample> = series
            .samples
            .iter()
            .filter(|s| s.timestamp_ms > range_start && s.timestamp_ms <= eval_ts)
            .copied()
            .collect();

        if !window.is_empty() {
            result.push(RangeSeries {
                labels: series.labels.clone(),
                samples: window,
            });
        }
    }
    Ok(Value::Matrix(result))
}
