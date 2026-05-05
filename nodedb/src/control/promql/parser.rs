//! Recursive descent PromQL parser.
//!
//! Parses a token stream (from [`super::lexer::tokenize`]) into an AST.
//! Operator precedence is handled via Pratt parsing for binary expressions.

use super::ast::*;
use super::error::PromqlError;
use super::label::{LabelMatchOp, LabelMatcher};
use super::lexer::Token;

/// Parse a PromQL expression from a token stream.
pub fn parse(tokens: &[Token]) -> Result<Expr, PromqlError> {
    let mut p = Parser::new(tokens);
    let expr = p.parse_expr(0)?;
    if !p.at_eof() {
        return Err(PromqlError::UnexpectedToken {
            expected: "end of expression".to_string(),
            found: format!("{:?}", p.peek()),
        });
    }
    Ok(expr)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> &Token {
        let tok = self.tokens.get(self.pos).unwrap_or(&Token::Eof);
        self.pos += 1;
        tok
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Token::Eof)
    }

    fn expect(&mut self, expected: &Token) -> Result<(), PromqlError> {
        let tok = self.advance().clone();
        if &tok == expected {
            Ok(())
        } else {
            Err(PromqlError::UnexpectedToken {
                expected: format!("{expected:?}"),
                found: format!("{tok:?}"),
            })
        }
    }

    /// Parse an expression with Pratt precedence.
    fn parse_expr(&mut self, min_prec: u8) -> Result<Expr, PromqlError> {
        let mut lhs = self.parse_unary()?;

        while let Some(op) = self.peek_binop() {
            if op.precedence() < min_prec {
                break;
            }
            self.advance();

            let return_bool = op.is_comparison() && self.try_keyword("bool");
            let matching = if op.is_set_op() {
                self.try_parse_set_matching()?
            } else {
                self.try_parse_vector_matching()?
            };

            let next_prec = if matches!(op, BinOp::Pow) {
                op.precedence() // right-associative
            } else {
                op.precedence() + 1
            };
            let rhs = self.parse_expr(next_prec)?;

            lhs = Expr::BinaryOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                return_bool,
                matching,
            };
        }

        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, PromqlError> {
        if matches!(self.peek(), Token::Sub) {
            self.advance();
            let expr = self.parse_unary()?;
            return Ok(Expr::Negate(Box::new(expr)));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, PromqlError> {
        let mut expr = self.parse_primary()?;

        if matches!(self.peek(), Token::LBracket) {
            self.advance();
            let range = self.parse_duration()?;
            let step = if matches!(self.peek(), Token::Colon) {
                self.advance();
                if matches!(self.peek(), Token::RBracket) {
                    None
                } else {
                    Some(self.parse_duration()?)
                }
            } else {
                None
            };
            self.expect(&Token::RBracket)?;

            if step.is_some() || !matches!(expr, Expr::VectorSelector { .. }) {
                expr = Expr::Subquery {
                    expr: Box::new(expr),
                    range,
                    step,
                };
            } else {
                expr = Expr::MatrixSelector {
                    selector: Box::new(expr),
                    range,
                };
            }
        }

        if self.try_keyword("offset") {
            let neg = matches!(self.peek(), Token::Sub);
            if neg {
                self.advance();
            }
            let mut dur = self.parse_duration()?;
            if neg {
                dur = Duration(-dur.ms());
            }
            expr = apply_offset(expr, dur);
        }

        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, PromqlError> {
        match self.peek().clone() {
            Token::Number(n) => {
                self.advance();
                Ok(Expr::Scalar(n))
            }
            Token::String(s) => {
                self.advance();
                Ok(Expr::StringLiteral(s))
            }
            Token::LParen => {
                self.advance();
                let expr = self.parse_expr(0)?;
                self.expect(&Token::RParen)?;
                Ok(Expr::Paren(Box::new(expr)))
            }
            Token::LBrace => {
                let matchers = self.parse_label_matchers()?;
                Ok(Expr::VectorSelector {
                    name: None,
                    matchers,
                    offset: None,
                })
            }
            Token::Ident(name) => {
                if let Some(agg_op) = parse_agg_op(&name) {
                    return self.parse_aggregation(agg_op);
                }
                self.advance();
                self.parse_ident_continuation(name)
            }
            other => Err(PromqlError::UnexpectedToken {
                expected: "expression".to_string(),
                found: format!("{other:?}"),
            }),
        }
    }

    fn parse_ident_continuation(&mut self, name: String) -> Result<Expr, PromqlError> {
        if matches!(self.peek(), Token::LParen) {
            return self.parse_function_call(name);
        }
        let matchers = if matches!(self.peek(), Token::LBrace) {
            self.parse_label_matchers()?
        } else {
            vec![]
        };
        Ok(Expr::VectorSelector {
            name: Some(name),
            matchers,
            offset: None,
        })
    }

    fn parse_function_call(&mut self, name: String) -> Result<Expr, PromqlError> {
        self.expect(&Token::LParen)?;
        let mut args = Vec::new();
        if !matches!(self.peek(), Token::RParen) {
            args.push(self.parse_expr(0)?);
            while matches!(self.peek(), Token::Comma) {
                self.advance();
                args.push(self.parse_expr(0)?);
            }
        }
        self.expect(&Token::RParen)?;
        Ok(Expr::Call { func: name, args })
    }

    fn parse_aggregation(&mut self, op: AggOp) -> Result<Expr, PromqlError> {
        self.advance();
        let grouping_before = self.try_parse_grouping()?;

        self.expect(&Token::LParen)?;
        let param = if matches!(
            op,
            AggOp::Topk | AggOp::Bottomk | AggOp::Quantile | AggOp::CountValues
        ) {
            let p = self.parse_expr(0)?;
            self.expect(&Token::Comma)?;
            Some(Box::new(p))
        } else {
            None
        };

        let expr = self.parse_expr(0)?;
        self.expect(&Token::RParen)?;

        let grouping = if matches!(grouping_before, Grouping::None) {
            self.try_parse_grouping()?
        } else {
            grouping_before
        };

        Ok(Expr::Aggregate {
            op,
            expr: Box::new(expr),
            param,
            grouping,
        })
    }

    fn parse_label_matchers(&mut self) -> Result<Vec<LabelMatcher>, PromqlError> {
        self.expect(&Token::LBrace)?;
        let mut matchers = Vec::new();
        if !matches!(self.peek(), Token::RBrace) {
            matchers.push(self.parse_one_matcher()?);
            while matches!(self.peek(), Token::Comma) {
                self.advance();
                if matches!(self.peek(), Token::RBrace) {
                    break;
                }
                matchers.push(self.parse_one_matcher()?);
            }
        }
        self.expect(&Token::RBrace)?;
        Ok(matchers)
    }

    fn parse_one_matcher(&mut self) -> Result<LabelMatcher, PromqlError> {
        let Token::Ident(name) = self.advance().clone() else {
            return Err(PromqlError::LabelMatcher {
                detail: "expected label name".to_string(),
            });
        };
        let op = match self.advance() {
            Token::Assign => LabelMatchOp::Equal,
            Token::Neq => LabelMatchOp::NotEqual,
            Token::MatchRegex => LabelMatchOp::RegexMatch,
            Token::NotMatchRegex => LabelMatchOp::RegexNotMatch,
            t => {
                return Err(PromqlError::LabelMatcher {
                    detail: format!("expected match operator, got {t:?}"),
                });
            }
        };
        let Token::String(value) = self.advance().clone() else {
            return Err(PromqlError::LabelMatcher {
                detail: "expected string value in label matcher".to_string(),
            });
        };
        Ok(LabelMatcher::new(name, op, value))
    }

    fn parse_duration(&mut self) -> Result<Duration, PromqlError> {
        match self.advance().clone() {
            Token::Duration(s) => {
                Duration::parse(&s).ok_or(PromqlError::InvalidDuration { literal: s })
            }
            Token::Number(n) if n > 0.0 => Ok(Duration((n * 1000.0) as i64)),
            t => Err(PromqlError::UnexpectedToken {
                expected: "duration".to_string(),
                found: format!("{t:?}"),
            }),
        }
    }

    fn try_parse_grouping(&mut self) -> Result<Grouping, PromqlError> {
        if self.try_keyword("by") {
            return Ok(Grouping::By(self.parse_label_list()?));
        }
        if self.try_keyword("without") {
            return Ok(Grouping::Without(self.parse_label_list()?));
        }
        Ok(Grouping::None)
    }

    fn parse_label_list(&mut self) -> Result<Vec<String>, PromqlError> {
        self.expect(&Token::LParen)?;
        let mut labels = Vec::new();
        if !matches!(self.peek(), Token::RParen) {
            if let Token::Ident(name) = self.advance().clone() {
                labels.push(name);
            }
            while matches!(self.peek(), Token::Comma) {
                self.advance();
                if let Token::Ident(name) = self.advance().clone() {
                    labels.push(name);
                }
            }
        }
        self.expect(&Token::RParen)?;
        Ok(labels)
    }

    fn try_keyword(&mut self, kw: &str) -> bool {
        if let Token::Ident(name) = self.peek()
            && name.eq_ignore_ascii_case(kw)
        {
            self.advance();
            return true;
        }
        false
    }

    fn peek_binop(&self) -> Option<BinOp> {
        match self.peek() {
            Token::Add => Some(BinOp::Add),
            Token::Sub => Some(BinOp::Sub),
            Token::Mul => Some(BinOp::Mul),
            Token::Div => Some(BinOp::Div),
            Token::Mod => Some(BinOp::Mod),
            Token::Pow => Some(BinOp::Pow),
            Token::Eq => Some(BinOp::Eq),
            Token::Neq => Some(BinOp::Neq),
            Token::Lt => Some(BinOp::Lt),
            Token::Gt => Some(BinOp::Gt),
            Token::Lte => Some(BinOp::Lte),
            Token::Gte => Some(BinOp::Gte),
            Token::Ident(s) if s.eq_ignore_ascii_case("and") => Some(BinOp::And),
            Token::Ident(s) if s.eq_ignore_ascii_case("or") => Some(BinOp::Or),
            Token::Ident(s) if s.eq_ignore_ascii_case("unless") => Some(BinOp::Unless),
            _ => None,
        }
    }

    fn try_parse_vector_matching(&mut self) -> Result<Option<VectorMatching>, PromqlError> {
        let has_on = self.try_keyword("on");
        let has_ignoring = !has_on && self.try_keyword("ignoring");
        if !has_on && !has_ignoring {
            return Ok(None);
        }
        let labels = self.parse_label_list()?;
        let mut matching = VectorMatching {
            card: MatchCard::OneToOne,
            on: if has_on { labels.clone() } else { vec![] },
            ignoring: if has_ignoring { labels } else { vec![] },
            group_left: vec![],
            group_right: vec![],
        };

        if self.try_keyword("group_left") {
            matching.card = MatchCard::ManyToOne;
            if matches!(self.peek(), Token::LParen) {
                matching.group_left = self.parse_label_list()?;
            }
        } else if self.try_keyword("group_right") {
            matching.card = MatchCard::OneToMany;
            if matches!(self.peek(), Token::LParen) {
                matching.group_right = self.parse_label_list()?;
            }
        }
        Ok(Some(matching))
    }

    fn try_parse_set_matching(&mut self) -> Result<Option<VectorMatching>, PromqlError> {
        let has_on = self.try_keyword("on");
        let has_ignoring = !has_on && self.try_keyword("ignoring");
        if !has_on && !has_ignoring {
            return Ok(None);
        }
        let labels = self.parse_label_list()?;
        Ok(Some(VectorMatching {
            card: MatchCard::OneToOne,
            on: if has_on { labels.clone() } else { vec![] },
            ignoring: if has_ignoring { labels } else { vec![] },
            group_left: vec![],
            group_right: vec![],
        }))
    }
}

fn apply_offset(expr: Expr, offset: Duration) -> Expr {
    match expr {
        Expr::VectorSelector { name, matchers, .. } => Expr::VectorSelector {
            name,
            matchers,
            offset: Some(offset),
        },
        other => other,
    }
}

fn parse_agg_op(name: &str) -> Option<AggOp> {
    match name.to_ascii_lowercase().as_str() {
        "sum" => Some(AggOp::Sum),
        "avg" => Some(AggOp::Avg),
        "min" => Some(AggOp::Min),
        "max" => Some(AggOp::Max),
        "count" => Some(AggOp::Count),
        "stddev" => Some(AggOp::Stddev),
        "stdvar" => Some(AggOp::Stdvar),
        "topk" => Some(AggOp::Topk),
        "bottomk" => Some(AggOp::Bottomk),
        "quantile" => Some(AggOp::Quantile),
        "count_values" => Some(AggOp::CountValues),
        "group" => Some(AggOp::Group),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::promql::lexer::tokenize;

    fn p(input: &str) -> Expr {
        let tokens = tokenize(input).unwrap();
        parse(&tokens).unwrap()
    }

    #[test]
    fn scalar_literal() {
        assert!(matches!(p("42"), Expr::Scalar(v) if (v - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn bare_metric() {
        assert!(matches!(p("up"), Expr::VectorSelector { name: Some(n), .. } if n == "up"));
    }

    #[test]
    fn metric_with_matchers() {
        let expr = p(r#"http_requests{method="GET", code=~"2.."}"#);
        if let Expr::VectorSelector { name, matchers, .. } = expr {
            assert_eq!(name, Some("http_requests".into()));
            assert_eq!(matchers.len(), 2);
        } else {
            panic!("expected VectorSelector");
        }
    }

    #[test]
    fn range_selector() {
        let expr = p("requests[5m]");
        assert!(matches!(expr, Expr::MatrixSelector { range, .. } if range.ms() == 300_000));
    }

    #[test]
    fn rate_function() {
        if let Expr::Call { func, args } = p("rate(requests[5m])") {
            assert_eq!(func, "rate");
            assert_eq!(args.len(), 1);
            assert!(matches!(&args[0], Expr::MatrixSelector { .. }));
        } else {
            panic!("expected Call");
        }
    }

    #[test]
    fn binary_precedence() {
        if let Expr::BinaryOp { op, rhs, .. } = p("a + b * c") {
            assert_eq!(op, BinOp::Add);
            assert!(matches!(*rhs, Expr::BinaryOp { op: BinOp::Mul, .. }));
        } else {
            panic!("expected BinaryOp");
        }
    }

    #[test]
    fn aggregation_by() {
        if let Expr::Aggregate { op, grouping, .. } = p("sum by (job) (rate(requests[5m]))") {
            assert_eq!(op, AggOp::Sum);
            assert_eq!(grouping, Grouping::By(vec!["job".into()]));
        } else {
            panic!("expected Aggregate");
        }
    }

    #[test]
    fn topk_with_param() {
        if let Expr::Aggregate { op, param, .. } = p("topk(5, requests)") {
            assert_eq!(op, AggOp::Topk);
            assert!(param.is_some());
        } else {
            panic!("expected Aggregate");
        }
    }

    #[test]
    fn nested_functions() {
        if let Expr::Call { func, args } =
            p("histogram_quantile(0.99, rate(http_duration_bucket[5m]))")
        {
            assert_eq!(func, "histogram_quantile");
            assert_eq!(args.len(), 2);
        } else {
            panic!("expected Call");
        }
    }

    #[test]
    fn unary_negate() {
        assert!(matches!(p("-up"), Expr::Negate(_)));
    }

    #[test]
    fn offset_modifier() {
        if let Expr::VectorSelector { offset, .. } = p("up offset 5m") {
            assert_eq!(offset, Some(Duration(300_000)));
        } else {
            panic!("expected VectorSelector");
        }
    }

    #[test]
    fn comparison_bool() {
        if let Expr::BinaryOp { return_bool, .. } = p("up > bool 0") {
            assert!(return_bool);
        } else {
            panic!("expected BinaryOp");
        }
    }
}
