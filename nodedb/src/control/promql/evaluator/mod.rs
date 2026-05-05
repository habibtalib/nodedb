//! PromQL expression evaluator.
//!
//! Evaluates a parsed AST against a set of pre-fetched time series.
//! Pure computation — the caller is responsible for data fetching.

mod aggregate;
mod binary;
mod call;
mod helpers;
mod selector;

use std::collections::BTreeMap;

use super::ast::*;
use super::error::PromqlError;
use super::types::*;

pub use helpers::{group_key, group_labels, labels_key, match_key};

/// Context for evaluation: pre-fetched series + query parameters.
pub struct EvalContext {
    /// All available time series (pre-fetched from storage).
    pub series: Vec<Series>,
    /// Evaluation timestamp for instant queries (milliseconds).
    pub timestamp_ms: i64,
    /// Lookback delta: how far back to search for a sample.
    pub lookback_ms: i64,
}

impl Default for EvalContext {
    fn default() -> Self {
        Self {
            series: vec![],
            timestamp_ms: 0,
            lookback_ms: DEFAULT_LOOKBACK_MS,
        }
    }
}

/// Evaluate a PromQL expression at a single timestamp (instant query).
pub fn evaluate_instant(ctx: &EvalContext, expr: &Expr) -> Result<Value, PromqlError> {
    eval(ctx, expr)
}

/// Evaluate a PromQL expression over a time range with steps (range query).
pub fn evaluate_range(
    ctx: &EvalContext,
    expr: &Expr,
    start_ms: i64,
    end_ms: i64,
    step_ms: i64,
) -> Result<Value, PromqlError> {
    let mut result_series: BTreeMap<String, RangeSeries> = BTreeMap::new();

    let mut ts = start_ms;
    while ts <= end_ms {
        let step_ctx = EvalContext {
            series: ctx.series.clone(),
            timestamp_ms: ts,
            lookback_ms: ctx.lookback_ms,
        };
        let val = eval(&step_ctx, expr)?;

        if let Value::Vector(samples) = val {
            for s in samples {
                let key = labels_key(&s.labels);
                let entry = result_series.entry(key).or_insert_with(|| RangeSeries {
                    labels: s.labels.clone(),
                    samples: vec![],
                });
                entry.samples.push(Sample {
                    timestamp_ms: ts,
                    value: s.value,
                });
            }
        } else if let Value::Scalar(v, _) = val {
            let key = "__scalar__".to_string();
            let entry = result_series.entry(key).or_insert_with(|| RangeSeries {
                labels: Labels::new(),
                samples: vec![],
            });
            entry.samples.push(Sample {
                timestamp_ms: ts,
                value: v,
            });
        }

        ts += step_ms;
    }

    Ok(Value::Matrix(result_series.into_values().collect()))
}

pub(crate) fn eval(ctx: &EvalContext, expr: &Expr) -> Result<Value, PromqlError> {
    match expr {
        Expr::Scalar(v) => Ok(Value::Scalar(*v, ctx.timestamp_ms)),
        Expr::StringLiteral(_) => Err(PromqlError::TypeError {
            context: "evaluation".to_string(),
            detail: "string literals not supported in evaluation".to_string(),
        }),
        Expr::VectorSelector {
            name,
            matchers,
            offset,
        } => selector::eval_vector_selector(ctx, name.as_deref(), matchers, *offset),
        Expr::MatrixSelector { selector, range } => {
            selector::eval_matrix_selector(ctx, selector, *range)
        }
        Expr::Paren(inner) => eval(ctx, inner),
        Expr::Negate(inner) => {
            let val = eval(ctx, inner)?;
            Ok(helpers::negate_value(val, ctx.timestamp_ms))
        }
        Expr::BinaryOp {
            op,
            lhs,
            rhs,
            return_bool,
            ..
        } => {
            let l = eval(ctx, lhs)?;
            let r = eval(ctx, rhs)?;
            binary::eval_binary_op(*op, l, r, *return_bool, ctx.timestamp_ms)
        }
        Expr::Aggregate {
            op,
            expr: inner,
            param,
            grouping,
        } => {
            let val = eval(ctx, inner)?;
            let p = match param {
                Some(p) => {
                    if let Value::Scalar(v, _) = eval(ctx, p)? {
                        Some(v)
                    } else {
                        None
                    }
                }
                None => None,
            };
            aggregate::eval_aggregation(*op, val, p, grouping, ctx.timestamp_ms)
        }
        Expr::Call { func, args } => call::eval_call(ctx, func, args),
        Expr::Subquery {
            expr: inner,
            range,
            step,
        } => eval_subquery(ctx, inner, *range, *step),
    }
}

/// Evaluate a subquery: `expr[range:step]`.
///
/// Evaluates the inner expression at each step within the range,
/// collecting results into a range vector (matrix).
fn eval_subquery(
    ctx: &EvalContext,
    inner: &Expr,
    range: Duration,
    step: Option<Duration>,
) -> Result<Value, PromqlError> {
    let end_ms = ctx.timestamp_ms;
    let start_ms = end_ms - range.ms();
    // Default step: evaluation interval, or 1 minute if unset.
    let step_ms = step.map_or(60_000, |d| d.ms()).max(1);

    let mut result_series: BTreeMap<String, RangeSeries> = BTreeMap::new();

    let mut ts = start_ms;
    while ts <= end_ms {
        let step_ctx = EvalContext {
            series: ctx.series.clone(),
            timestamp_ms: ts,
            lookback_ms: ctx.lookback_ms,
        };
        let val = eval(&step_ctx, inner)?;

        if let Value::Vector(samples) = val {
            for s in samples {
                let key = labels_key(&s.labels);
                let entry = result_series.entry(key).or_insert_with(|| RangeSeries {
                    labels: s.labels.clone(),
                    samples: vec![],
                });
                entry.samples.push(Sample {
                    timestamp_ms: ts,
                    value: s.value,
                });
            }
        }

        ts += step_ms;
    }

    Ok(Value::Matrix(result_series.into_values().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::promql::lexer::tokenize;
    use crate::control::promql::parser;

    fn eval_query(query: &str, series: Vec<Series>, ts: i64) -> Value {
        let tokens = tokenize(query).unwrap();
        let expr = parser::parse(&tokens).unwrap();
        let ctx = EvalContext {
            series,
            timestamp_ms: ts,
            lookback_ms: DEFAULT_LOOKBACK_MS,
        };
        evaluate_instant(&ctx, &expr).unwrap()
    }

    fn make_series(name: &str, labels: &[(&str, &str)], samples: &[(i64, f64)]) -> Series {
        let mut l = Labels::new();
        l.insert("__name__".into(), name.into());
        for (k, v) in labels {
            l.insert(k.to_string(), v.to_string());
        }
        Series {
            labels: l,
            samples: samples
                .iter()
                .map(|&(t, v)| Sample {
                    timestamp_ms: t,
                    value: v,
                })
                .collect(),
        }
    }

    #[test]
    fn scalar_eval() {
        let val = eval_query("42", vec![], 1000);
        assert!(matches!(val, Value::Scalar(v, _) if (v - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn vector_selector() {
        let series = vec![make_series(
            "up",
            &[("job", "api")],
            &[(900, 1.0), (1000, 1.0)],
        )];
        let val = eval_query("up", series, 1000);
        if let Value::Vector(v) = val {
            assert_eq!(v.len(), 1);
            assert!((v[0].value - 1.0).abs() < f64::EPSILON);
        } else {
            panic!("expected vector");
        }
    }

    #[test]
    fn rate_function_eval() {
        let series = vec![make_series(
            "requests",
            &[("job", "api")],
            &[(0, 0.0), (1000, 10.0), (2000, 20.0), (3000, 30.0)],
        )];
        let val = eval_query("rate(requests[5m])", series, 3000);
        if let Value::Vector(v) = val {
            assert_eq!(v.len(), 1);
            assert!((v[0].value - 10.0).abs() < 1e-9);
        } else {
            panic!("expected vector");
        }
    }

    #[test]
    fn binary_scalar_op() {
        let series = vec![make_series("cpu", &[], &[(1000, 0.8)])];
        let val = eval_query("cpu * 100", series, 1000);
        if let Value::Vector(v) = val {
            assert_eq!(v.len(), 1);
            assert!((v[0].value - 80.0).abs() < 1e-9);
        } else {
            panic!("expected vector");
        }
    }

    #[test]
    fn sum_aggregation() {
        let series = vec![
            make_series("requests", &[("job", "api")], &[(1000, 10.0)]),
            make_series("requests", &[("job", "web")], &[(1000, 20.0)]),
        ];
        let val = eval_query("sum(requests)", series, 1000);
        if let Value::Vector(v) = val {
            assert_eq!(v.len(), 1);
            assert!((v[0].value - 30.0).abs() < 1e-9);
        } else {
            panic!("expected vector");
        }
    }

    #[test]
    fn sum_by_aggregation() {
        let series = vec![
            make_series(
                "requests",
                &[("job", "api"), ("env", "prod")],
                &[(1000, 10.0)],
            ),
            make_series(
                "requests",
                &[("job", "web"), ("env", "prod")],
                &[(1000, 20.0)],
            ),
            make_series(
                "requests",
                &[("job", "api"), ("env", "dev")],
                &[(1000, 5.0)],
            ),
        ];
        let val = eval_query("sum by (env) (requests)", series, 1000);
        if let Value::Vector(v) = val {
            assert_eq!(v.len(), 2);
            let prod = v
                .iter()
                .find(|s| s.labels.get("env") == Some(&"prod".into()));
            assert!((prod.unwrap().value - 30.0).abs() < 1e-9);
        } else {
            panic!("expected vector");
        }
    }

    #[test]
    fn range_query() {
        let series = vec![make_series(
            "up",
            &[("job", "api")],
            &[(1000, 1.0), (2000, 1.0), (3000, 0.0)],
        )];
        let tokens = tokenize("up").unwrap();
        let expr = parser::parse(&tokens).unwrap();
        let ctx = EvalContext {
            series,
            timestamp_ms: 0,
            lookback_ms: DEFAULT_LOOKBACK_MS,
        };
        let val = evaluate_range(&ctx, &expr, 1000, 3000, 1000).unwrap();
        if let Value::Matrix(m) = val {
            assert_eq!(m.len(), 1);
            assert_eq!(m[0].samples.len(), 3);
        } else {
            panic!("expected matrix");
        }
    }
}
