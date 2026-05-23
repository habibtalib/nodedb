// SPDX-License-Identifier: BUSL-1.1

//! sqlparser AST → internal `VSelect` representation.

use sqlparser::ast::{
    BinaryOperator, Expr as SqlExpr, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr,
    LimitClause, OrderByExpr, OrderByKind, Query, SelectItem, SetExpr, Statement, UnaryOperator,
    Value,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use super::expr::{AggFn, BinOp, EvalError, Expr};
use super::value::VValue;

#[derive(Debug, Clone)]
pub struct VSelect {
    pub projection: Vec<VProj>,
    pub filter: Option<Expr>,
    pub order_by: Vec<(Expr, bool /*asc*/)>,
    pub limit: Option<usize>,
    pub offset: usize,
    /// True if any projection item is a top-level aggregate. The whole
    /// projection is then evaluated once over the row set (no GROUP BY: a
    /// single implicit group spanning all rows).
    pub has_aggregate: bool,
}

#[derive(Debug, Clone)]
pub enum VProj {
    Star,
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("unsupported on virtual catalog tables: {0}")]
    Unsupported(String),
    #[error("eval error: {0}")]
    Eval(#[from] EvalError),
}

/// Parse the client SQL and extract the SELECT body. Returns `Ok(None)` if
/// the statement is not a single plain SELECT (e.g. multi-statement,
/// CTE-only, INSERT) so the caller can fall through to its non-virtual
/// path.
pub fn parse_select(sql: &str) -> Result<VSelect, ParseError> {
    parse_select_with_params(sql, &[])
}

/// Parse a SELECT, binding `$N` placeholders to concrete values from `params`
/// before lowering. Unbound placeholders surface as a parse error (catalog
/// queries are always fully bound by the time Execute runs).
pub fn parse_select_with_params(
    sql: &str,
    params: &[nodedb_sql::ParamValue],
) -> Result<VSelect, ParseError> {
    let dialect = PostgreSqlDialect {};
    let mut stmts =
        Parser::parse_sql(&dialect, sql).map_err(|e| ParseError::Parse(e.to_string()))?;
    if stmts.len() != 1 {
        return Err(ParseError::Unsupported(
            "expected exactly one SQL statement".into(),
        ));
    }
    let mut stmt = stmts.pop().unwrap();
    if !params.is_empty() {
        nodedb_sql::params::bind_params(&mut stmt, params);
    }
    let Statement::Query(query) = stmt else {
        return Err(ParseError::Unsupported(
            "expected a SELECT statement".into(),
        ));
    };
    select_from_query(*query)
}

fn select_from_query(query: Query) -> Result<VSelect, ParseError> {
    if query.with.is_some() {
        return Err(ParseError::Unsupported("WITH (CTE) not supported".into()));
    }
    let SetExpr::Select(select) = *query.body else {
        return Err(ParseError::Unsupported(
            "compound SELECT (UNION/INTERSECT/EXCEPT) not supported".into(),
        ));
    };

    let group_by_empty = matches!(
        &select.group_by,
        GroupByExpr::Expressions(exprs, mods) if exprs.is_empty() && mods.is_empty()
    );
    if !group_by_empty {
        return Err(ParseError::Unsupported("GROUP BY not supported".into()));
    }
    if select.having.is_some() {
        return Err(ParseError::Unsupported("HAVING not supported".into()));
    }
    if select.distinct.is_some() {
        return Err(ParseError::Unsupported("DISTINCT not supported".into()));
    }

    let mut projection = Vec::with_capacity(select.projection.len());
    let mut has_aggregate = false;
    for item in select.projection {
        match item {
            SelectItem::Wildcard(_) => projection.push(VProj::Star),
            SelectItem::UnnamedExpr(e) => {
                let expr = lower_expr(e)?;
                if matches!(expr, Expr::Aggregate(_, _)) {
                    has_aggregate = true;
                }
                projection.push(VProj::Expr { expr, alias: None });
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                let expr = lower_expr(expr)?;
                if matches!(expr, Expr::Aggregate(_, _)) {
                    has_aggregate = true;
                }
                projection.push(VProj::Expr {
                    expr,
                    alias: Some(alias.value),
                });
            }
            SelectItem::QualifiedWildcard(_, _) => {
                return Err(ParseError::Unsupported(
                    "qualified wildcard (t.*) not supported".into(),
                ));
            }
        }
    }

    let filter = match select.selection {
        Some(e) => Some(lower_expr(e)?),
        None => None,
    };

    let mut order_by_items: Vec<(Expr, bool)> = Vec::new();
    if let Some(ob) = query.order_by {
        match ob.kind {
            OrderByKind::Expressions(exprs) => {
                for OrderByExpr { expr, options, .. } in exprs {
                    order_by_items.push((lower_expr(expr)?, options.asc.unwrap_or(true)));
                }
            }
            OrderByKind::All(_) => {
                return Err(ParseError::Unsupported(
                    "ORDER BY ALL not supported on virtual tables".into(),
                ));
            }
        }
    }

    let (limit, offset) = match query.limit_clause {
        None => (None, 0usize),
        Some(LimitClause::LimitOffset {
            limit,
            offset,
            limit_by,
        }) => {
            if !limit_by.is_empty() {
                return Err(ParseError::Unsupported(
                    "LIMIT BY not supported on virtual tables".into(),
                ));
            }
            let lim = match limit {
                Some(e) => Some(literal_usize(e)?),
                None => None,
            };
            let off = match offset {
                Some(o) => literal_usize(o.value)?,
                None => 0,
            };
            (lim, off)
        }
        Some(LimitClause::OffsetCommaLimit { offset, limit }) => {
            (Some(literal_usize(limit)?), literal_usize(offset)?)
        }
    };

    Ok(VSelect {
        projection,
        filter,
        order_by: order_by_items,
        limit,
        offset,
        has_aggregate,
    })
}

fn literal_usize(e: SqlExpr) -> Result<usize, ParseError> {
    let v = lower_expr(e)?;
    let n = match v {
        Expr::Literal(VValue::Int4(i)) if i >= 0 => i as usize,
        Expr::Literal(VValue::Int8(i)) if i >= 0 => i as usize,
        _ => {
            return Err(ParseError::Unsupported(
                "LIMIT/OFFSET must be a non-negative integer literal".into(),
            ));
        }
    };
    Ok(n)
}

fn lower_expr(e: SqlExpr) -> Result<Expr, ParseError> {
    match e {
        SqlExpr::Value(v) => Ok(Expr::Literal(lower_literal(v.value)?)),
        SqlExpr::Identifier(id) => Ok(Expr::Column(id.value)),
        SqlExpr::CompoundIdentifier(ids) => {
            // Strip optional table qualifier — virtual tables have a single
            // FROM relation, so the last component is the column name.
            let last = ids
                .last()
                .ok_or_else(|| ParseError::Unsupported("empty compound identifier".into()))?;
            Ok(Expr::Column(last.value.clone()))
        }
        SqlExpr::Nested(inner) => lower_expr(*inner),
        SqlExpr::UnaryOp { op, expr } => {
            let inner = Box::new(lower_expr(*expr)?);
            match op {
                UnaryOperator::Not => Ok(Expr::UnaryNot(inner)),
                UnaryOperator::Minus => Ok(Expr::UnaryNeg(inner)),
                UnaryOperator::Plus => Ok(*inner),
                other => Err(ParseError::Unsupported(format!("unary op {other:?}"))),
            }
        }
        SqlExpr::BinaryOp { left, op, right } => {
            let bop = match op {
                BinaryOperator::Eq => BinOp::Eq,
                BinaryOperator::NotEq => BinOp::NotEq,
                BinaryOperator::Lt => BinOp::Lt,
                BinaryOperator::LtEq => BinOp::LtEq,
                BinaryOperator::Gt => BinOp::Gt,
                BinaryOperator::GtEq => BinOp::GtEq,
                BinaryOperator::And => BinOp::And,
                BinaryOperator::Or => BinOp::Or,
                BinaryOperator::Plus => BinOp::Add,
                BinaryOperator::Minus => BinOp::Sub,
                BinaryOperator::Multiply => BinOp::Mul,
                BinaryOperator::Divide => BinOp::Div,
                other => return Err(ParseError::Unsupported(format!("binary op {other:?}"))),
            };
            Ok(Expr::BinaryOp(
                Box::new(lower_expr(*left)?),
                bop,
                Box::new(lower_expr(*right)?),
            ))
        }
        SqlExpr::IsNull(e) => Ok(Expr::IsNull(Box::new(lower_expr(*e)?), false)),
        SqlExpr::IsNotNull(e) => Ok(Expr::IsNull(Box::new(lower_expr(*e)?), true)),
        SqlExpr::IsTrue(e) => Ok(Expr::BinaryOp(
            Box::new(lower_expr(*e)?),
            BinOp::Eq,
            Box::new(Expr::Literal(VValue::Bool(true))),
        )),
        SqlExpr::IsFalse(e) => Ok(Expr::BinaryOp(
            Box::new(lower_expr(*e)?),
            BinOp::Eq,
            Box::new(Expr::Literal(VValue::Bool(false))),
        )),
        SqlExpr::InList {
            expr,
            list,
            negated,
        } => {
            let items = list
                .into_iter()
                .map(lower_expr)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Expr::InList(Box::new(lower_expr(*expr)?), items, negated))
        }
        SqlExpr::Between {
            expr,
            negated,
            low,
            high,
        } => Ok(Expr::Between(
            Box::new(lower_expr(*expr)?),
            Box::new(lower_expr(*low)?),
            Box::new(lower_expr(*high)?),
            negated,
        )),
        SqlExpr::Like {
            negated,
            expr,
            pattern,
            escape_char: _,
            any: _,
        } => {
            let pat_val = lower_expr(*pattern)?;
            let Expr::Literal(VValue::Text(s)) = pat_val else {
                return Err(ParseError::Unsupported(
                    "LIKE pattern must be a string literal".into(),
                ));
            };
            Ok(Expr::Like(Box::new(lower_expr(*expr)?), s, negated))
        }
        SqlExpr::Function(func) => lower_function(func),
        other => Err(ParseError::Unsupported(format!(
            "expression {other:?} not supported on virtual catalog tables"
        ))),
    }
}

fn lower_function(func: sqlparser::ast::Function) -> Result<Expr, ParseError> {
    let name = func
        .name
        .0
        .last()
        .map(|p| match p {
            sqlparser::ast::ObjectNamePart::Identifier(id) => id.value.to_ascii_lowercase(),
            sqlparser::ast::ObjectNamePart::Function(_) => String::new(),
        })
        .unwrap_or_default();

    let agg = match name.as_str() {
        "count" => AggFn::Count,
        "sum" => AggFn::Sum,
        "min" => AggFn::Min,
        "max" => AggFn::Max,
        "avg" => AggFn::Avg,
        _ => {
            return Err(ParseError::Unsupported(format!(
                "function `{name}` not supported on virtual catalog tables"
            )));
        }
    };

    let args = match func.args {
        FunctionArguments::List(list) => list.args,
        FunctionArguments::None => Vec::new(),
        FunctionArguments::Subquery(_) => {
            return Err(ParseError::Unsupported(
                "subquery as function argument not supported".into(),
            ));
        }
    };

    if args.len() != 1 {
        return Err(ParseError::Unsupported(format!(
            "aggregate `{name}` expects exactly one argument"
        )));
    }
    let arg_expr = match args.into_iter().next().unwrap() {
        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => Expr::Star,
        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => lower_expr(e)?,
        FunctionArg::Unnamed(FunctionArgExpr::QualifiedWildcard(_)) => Expr::Star,
        FunctionArg::Named { .. } | FunctionArg::ExprNamed { .. } => {
            return Err(ParseError::Unsupported(
                "named function arguments not supported on virtual catalog tables".into(),
            ));
        }
    };
    Ok(Expr::Aggregate(agg, Box::new(arg_expr)))
}

fn lower_literal(v: Value) -> Result<VValue, ParseError> {
    match v {
        Value::Null => Ok(VValue::Null),
        Value::Boolean(b) => Ok(VValue::Bool(b)),
        Value::Number(s, _) => {
            if let Ok(i) = s.parse::<i64>() {
                Ok(VValue::Int8(i))
            } else {
                Err(ParseError::Unsupported(format!(
                    "non-integer numeric literal `{s}` not supported on virtual tables"
                )))
            }
        }
        // Unbound `$N` placeholders: only reachable on the Parse/Describe
        // path before parameters are bound. Treat as NULL so schema inference
        // succeeds. Execute always re-parses with `bind_params` first.
        Value::Placeholder(_) => Ok(VValue::Null),
        Value::SingleQuotedString(s)
        | Value::DoubleQuotedString(s)
        | Value::EscapedStringLiteral(s)
        | Value::NationalStringLiteral(s)
        | Value::DollarQuotedString(sqlparser::ast::DollarQuotedString { value: s, .. }) => {
            Ok(VValue::Text(s))
        }
        other => Err(ParseError::Unsupported(format!(
            "literal value {other:?} not supported on virtual catalog tables"
        ))),
    }
}
