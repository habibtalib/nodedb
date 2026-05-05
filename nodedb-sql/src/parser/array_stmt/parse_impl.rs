//! `Parser` struct and recursive-descent parse methods for array statements.

use super::super::ast::{
    AlterArrayAst, CreateArrayAst, DeleteArrayAst, DropArrayAst, InsertArrayAst,
};
use super::super::lexer::{Tok, Token};
use crate::error::{Result, SqlError};
use crate::types_array::{
    ArrayAttrAst, ArrayAttrLiteral, ArrayAttrType, ArrayCellOrderAst, ArrayCoordLiteral,
    ArrayDimAst, ArrayDimType, ArrayDomainBound, ArrayInsertRow, ArrayTileOrderAst,
};

pub(super) struct Parser<'a> {
    pub(super) toks: &'a [Token],
    pub(super) i: usize,
}

impl<'a> Parser<'a> {
    pub(super) fn new(toks: &'a [Token]) -> Self {
        Self { toks, i: 0 }
    }

    pub(super) fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i).map(|t| &t.tok)
    }

    pub(super) fn bump(&mut self) -> Option<&'a Token> {
        let t = self.toks.get(self.i)?;
        self.i += 1;
        Some(t)
    }

    pub(super) fn at_end(&self) -> bool {
        self.i >= self.toks.len()
    }

    pub(super) fn err(&self, msg: impl Into<String>) -> SqlError {
        SqlError::Parse { detail: msg.into() }
    }

    pub(super) fn expect_kw(&mut self, kw: &str) -> Result<()> {
        match self.peek() {
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(kw) => {
                self.i += 1;
                Ok(())
            }
            other => Err(self.err(format!("expected keyword `{kw}`, got {other:?}"))),
        }
    }

    pub(super) fn match_kw(&mut self, kw: &str) -> bool {
        match self.peek() {
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(kw) => {
                self.i += 1;
                true
            }
            _ => false,
        }
    }

    pub(super) fn expect_ident(&mut self) -> Result<String> {
        match self.bump().map(|t| &t.tok) {
            Some(Tok::Ident(s)) => Ok(s.clone()),
            other => Err(self.err(format!("expected identifier, got {other:?}"))),
        }
    }

    pub(super) fn expect(&mut self, want: &Tok) -> Result<()> {
        if self.peek() == Some(want) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(format!("expected {want:?}, got {:?}", self.peek())))
        }
    }

    pub(super) fn match_token(&mut self, want: &Tok) -> bool {
        if self.peek() == Some(want) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    pub(super) fn expect_int(&mut self) -> Result<i64> {
        match self.bump().map(|t| &t.tok) {
            Some(Tok::Int(n)) => Ok(*n),
            other => Err(self.err(format!("expected integer, got {other:?}"))),
        }
    }

    pub(super) fn expect_float_or_int_as_f64(&mut self) -> Result<f64> {
        match self.bump().map(|t| &t.tok) {
            Some(Tok::Int(n)) => Ok(*n as f64),
            Some(Tok::Float(f)) => Ok(*f),
            other => Err(self.err(format!("expected number, got {other:?}"))),
        }
    }

    // ── CREATE ARRAY ─────────────────────────────────────────────

    pub(super) fn parse_create(&mut self) -> Result<CreateArrayAst> {
        let name = self.expect_ident()?;
        self.expect_kw("DIMS")?;
        self.expect(&Tok::LParen)?;
        let mut dims = Vec::new();
        loop {
            dims.push(self.parse_dim()?);
            if !self.match_token(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;

        self.expect_kw("ATTRS")?;
        self.expect(&Tok::LParen)?;
        let mut attrs = Vec::new();
        loop {
            attrs.push(self.parse_attr()?);
            if !self.match_token(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;

        self.expect_kw("TILE_EXTENTS")?;
        self.expect(&Tok::LParen)?;
        let mut tile_extents = Vec::new();
        loop {
            tile_extents.push(self.expect_int()?);
            if !self.match_token(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;

        let mut cell_order = ArrayCellOrderAst::default();
        let mut tile_order = ArrayTileOrderAst::default();
        if self.match_kw("CELL_ORDER") {
            cell_order = self.parse_cell_order()?;
        }
        if self.match_kw("TILE_ORDER") {
            tile_order = self.parse_tile_order()?;
        }

        // Optional `WITH (key = value, ...)` clause.
        let mut prefix_bits: u8 = 8;
        let mut audit_retain_ms: Option<u64> = None;
        let mut minimum_audit_retain_ms: Option<u64> = None;
        if self.match_kw("WITH") {
            self.expect(&Tok::LParen)?;
            loop {
                let key = self.expect_ident()?;
                self.expect(&Tok::Eq)?;
                match key.to_ascii_lowercase().as_str() {
                    "prefix_bits" => {
                        let n = self.expect_int()?;
                        if !(1..=16).contains(&n) {
                            return Err(self.err(format!("WITH (prefix_bits = {n}): must be 1–16")));
                        }
                        prefix_bits = n as u8;
                    }
                    "audit_retain_ms" => {
                        let n = self.expect_int()?;
                        if n < 0 {
                            return Err(
                                self.err(format!("WITH (audit_retain_ms = {n}): must be >= 0"))
                            );
                        }
                        audit_retain_ms = Some(n as u64);
                    }
                    "minimum_audit_retain_ms" => {
                        let n = self.expect_int()?;
                        if n < 0 {
                            return Err(self.err(format!(
                                "WITH (minimum_audit_retain_ms = {n}): must be >= 0"
                            )));
                        }
                        minimum_audit_retain_ms = Some(n as u64);
                    }
                    other => {
                        return Err(self.err(format!(
                            "WITH: unknown option `{other}`; expected one of \
                             `prefix_bits`, `audit_retain_ms`, `minimum_audit_retain_ms`"
                        )));
                    }
                }
                if !self.match_token(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
        }

        if !self.at_end() {
            return Err(self.err(format!(
                "trailing tokens after CREATE ARRAY: {:?}",
                self.peek()
            )));
        }

        Ok(CreateArrayAst {
            name,
            dims,
            attrs,
            tile_extents,
            cell_order,
            tile_order,
            prefix_bits,
            audit_retain_ms,
            minimum_audit_retain_ms,
        })
    }

    fn parse_dim(&mut self) -> Result<ArrayDimAst> {
        let name = self.expect_ident()?;
        let type_name = self.expect_ident()?;
        let dtype = parse_dim_type(&type_name)
            .ok_or_else(|| self.err(format!("unknown dim type `{type_name}`")))?;
        // Domain bounds [lo..hi] are optional — omitting them defaults to the
        // full representable range for the dim type.
        let (lo, hi) = if self.match_token(&Tok::LBracket) {
            let lo = self.parse_domain_bound(dtype)?;
            self.expect(&Tok::DotDot)?;
            let hi = self.parse_domain_bound(dtype)?;
            self.expect(&Tok::RBracket)?;
            (lo, hi)
        } else {
            let (lo, hi) = default_domain_bounds(dtype);
            (lo, hi)
        };
        Ok(ArrayDimAst {
            name,
            dtype,
            lo,
            hi,
        })
    }

    fn parse_domain_bound(&mut self, dtype: ArrayDimType) -> Result<ArrayDomainBound> {
        match dtype {
            ArrayDimType::Int64 => Ok(ArrayDomainBound::Int64(self.expect_int()?)),
            ArrayDimType::TimestampMs => Ok(ArrayDomainBound::TimestampMs(self.expect_int()?)),
            ArrayDimType::Float64 => Ok(ArrayDomainBound::Float64(
                self.expect_float_or_int_as_f64()?,
            )),
            ArrayDimType::String => match self.bump().map(|t| &t.tok) {
                Some(Tok::Str(s)) => Ok(ArrayDomainBound::String(s.clone())),
                other => Err(self.err(format!("expected string literal, got {other:?}"))),
            },
        }
    }

    fn parse_attr(&mut self) -> Result<ArrayAttrAst> {
        let name = self.expect_ident()?;
        let type_name = self.expect_ident()?;
        let dtype = parse_attr_type(&type_name)
            .ok_or_else(|| self.err(format!("unknown attr type `{type_name}`")))?;
        let nullable = if self.match_kw("NOT") {
            self.expect_kw("NULL")?;
            false
        } else {
            // Default: nullable.
            true
        };
        Ok(ArrayAttrAst {
            name,
            dtype,
            nullable,
        })
    }

    fn parse_cell_order(&mut self) -> Result<ArrayCellOrderAst> {
        let id = self.expect_ident()?;
        match id.to_ascii_uppercase().as_str() {
            "ROW_MAJOR" => Ok(ArrayCellOrderAst::RowMajor),
            "COL_MAJOR" => Ok(ArrayCellOrderAst::ColMajor),
            "HILBERT" => Ok(ArrayCellOrderAst::Hilbert),
            "ZORDER" | "Z_ORDER" => Ok(ArrayCellOrderAst::ZOrder),
            other => Err(self.err(format!("unknown CELL_ORDER `{other}`"))),
        }
    }

    fn parse_tile_order(&mut self) -> Result<ArrayTileOrderAst> {
        let id = self.expect_ident()?;
        match id.to_ascii_uppercase().as_str() {
            "ROW_MAJOR" => Ok(ArrayTileOrderAst::RowMajor),
            "COL_MAJOR" => Ok(ArrayTileOrderAst::ColMajor),
            "HILBERT" => Ok(ArrayTileOrderAst::Hilbert),
            "ZORDER" | "Z_ORDER" => Ok(ArrayTileOrderAst::ZOrder),
            other => Err(self.err(format!("unknown TILE_ORDER `{other}`"))),
        }
    }

    // ── DROP ARRAY ───────────────────────────────────────────────

    pub(super) fn parse_drop(&mut self) -> Result<DropArrayAst> {
        let if_exists = if self.match_kw("IF") {
            self.expect_kw("EXISTS")?;
            true
        } else {
            false
        };
        let name = self.expect_ident()?;
        if !self.at_end() {
            return Err(self.err(format!(
                "trailing tokens after DROP ARRAY: {:?}",
                self.peek()
            )));
        }
        Ok(DropArrayAst { name, if_exists })
    }

    // ── INSERT INTO ARRAY ────────────────────────────────────────

    pub(super) fn parse_insert(&mut self) -> Result<InsertArrayAst> {
        let name = self.expect_ident()?;
        let mut rows = Vec::new();
        loop {
            self.expect_kw("COORDS")?;
            self.expect(&Tok::LParen)?;
            let mut coords = Vec::new();
            loop {
                coords.push(self.parse_coord_literal()?);
                if !self.match_token(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;

            self.expect_kw("VALUES")?;
            self.expect(&Tok::LParen)?;
            let mut attrs = Vec::new();
            loop {
                attrs.push(self.parse_attr_literal()?);
                if !self.match_token(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;

            rows.push(ArrayInsertRow { coords, attrs });
            if !self.match_token(&Tok::Comma) {
                break;
            }
        }
        if !self.at_end() {
            return Err(self.err(format!(
                "trailing tokens after INSERT INTO ARRAY: {:?}",
                self.peek()
            )));
        }
        Ok(InsertArrayAst { name, rows })
    }

    fn parse_coord_literal(&mut self) -> Result<ArrayCoordLiteral> {
        match self.bump().map(|t| &t.tok) {
            Some(Tok::Int(n)) => Ok(ArrayCoordLiteral::Int64(*n)),
            Some(Tok::Float(f)) => Ok(ArrayCoordLiteral::Float64(*f)),
            Some(Tok::Str(s)) => Ok(ArrayCoordLiteral::String(s.clone())),
            other => Err(self.err(format!("expected coord literal, got {other:?}"))),
        }
    }

    fn parse_attr_literal(&mut self) -> Result<ArrayAttrLiteral> {
        match self.bump().map(|t| &t.tok) {
            Some(Tok::Null) => Ok(ArrayAttrLiteral::Null),
            Some(Tok::Int(n)) => Ok(ArrayAttrLiteral::Int64(*n)),
            Some(Tok::Float(f)) => Ok(ArrayAttrLiteral::Float64(*f)),
            Some(Tok::Str(s)) => Ok(ArrayAttrLiteral::String(s.clone())),
            other => Err(self.err(format!("expected attr literal, got {other:?}"))),
        }
    }

    // ── ALTER NDARRAY ────────────────────────────────────────────

    pub(super) fn parse_alter(&mut self) -> Result<AlterArrayAst> {
        let name = self.expect_ident()?;
        self.expect_kw("SET")?;
        self.expect(&Tok::LParen)?;
        let mut set = Vec::new();
        loop {
            let key = self.expect_ident()?;
            self.expect(&Tok::Eq)?;
            let value = match self.peek() {
                Some(Tok::Null) => {
                    self.i += 1;
                    None
                }
                Some(Tok::Int(_)) => {
                    let n = self.expect_int()?;
                    Some(n)
                }
                other => {
                    return Err(self.err(format!(
                        "SET {key}: expected integer or NULL, got {other:?}"
                    )));
                }
            };
            let key_lower = key.to_ascii_lowercase();
            match key_lower.as_str() {
                "audit_retain_ms" => {
                    if let Some(n) = value
                        && n < 0
                    {
                        return Err(
                            self.err(format!("SET audit_retain_ms = {n}: must be >= 0 or NULL"))
                        );
                    }
                }
                "minimum_audit_retain_ms" => match value {
                    Some(n) if n < 0 => {
                        return Err(
                            self.err(format!("SET minimum_audit_retain_ms = {n}: must be >= 0"))
                        );
                    }
                    None => {
                        return Err(self.err(
                            "SET minimum_audit_retain_ms = NULL: floor cannot be NULL".to_string(),
                        ));
                    }
                    _ => {}
                },
                other => {
                    return Err(self.err(format!(
                        "SET: unknown key `{other}`; expected `audit_retain_ms` \
                         or `minimum_audit_retain_ms`"
                    )));
                }
            }
            set.push((key_lower, value));
            if !self.match_token(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        if !self.at_end() {
            return Err(self.err(format!(
                "trailing tokens after ALTER NDARRAY: {:?}",
                self.peek()
            )));
        }
        if set.is_empty() {
            return Err(self.err("ALTER NDARRAY SET (): at least one key is required"));
        }
        Ok(AlterArrayAst { name, set })
    }

    // ── DELETE FROM ARRAY ────────────────────────────────────────

    pub(super) fn parse_delete(&mut self) -> Result<DeleteArrayAst> {
        let name = self.expect_ident()?;
        self.expect_kw("WHERE")?;
        self.expect_kw("COORDS")?;
        self.expect_kw("IN")?;
        self.expect(&Tok::LParen)?;
        let mut coords = Vec::new();
        loop {
            self.expect(&Tok::LParen)?;
            let mut row = Vec::new();
            loop {
                row.push(self.parse_coord_literal()?);
                if !self.match_token(&Tok::Comma) {
                    break;
                }
            }
            self.expect(&Tok::RParen)?;
            coords.push(row);
            if !self.match_token(&Tok::Comma) {
                break;
            }
        }
        self.expect(&Tok::RParen)?;
        if !self.at_end() {
            return Err(self.err(format!(
                "trailing tokens after DELETE FROM ARRAY: {:?}",
                self.peek()
            )));
        }
        Ok(DeleteArrayAst { name, coords })
    }
}

fn default_domain_bounds(dtype: ArrayDimType) -> (ArrayDomainBound, ArrayDomainBound) {
    match dtype {
        ArrayDimType::Int64 => (
            ArrayDomainBound::Int64(i64::MIN),
            ArrayDomainBound::Int64(i64::MAX),
        ),
        ArrayDimType::Float64 => (
            ArrayDomainBound::Float64(f64::MIN),
            ArrayDomainBound::Float64(f64::MAX),
        ),
        ArrayDimType::TimestampMs => (
            ArrayDomainBound::TimestampMs(0),
            ArrayDomainBound::TimestampMs(i64::MAX),
        ),
        ArrayDimType::String => (
            ArrayDomainBound::String(String::new()),
            ArrayDomainBound::String("\u{FFFF}".repeat(8)),
        ),
    }
}

pub(super) fn parse_dim_type(s: &str) -> Option<ArrayDimType> {
    match s.to_ascii_uppercase().as_str() {
        "INT64" | "INT" | "BIGINT" => Some(ArrayDimType::Int64),
        "FLOAT64" | "DOUBLE" | "FLOAT" => Some(ArrayDimType::Float64),
        "TIMESTAMP_MS" | "TIMESTAMPMS" => Some(ArrayDimType::TimestampMs),
        "STRING" | "TEXT" | "VARCHAR" => Some(ArrayDimType::String),
        _ => None,
    }
}

pub(super) fn parse_attr_type(s: &str) -> Option<ArrayAttrType> {
    match s.to_ascii_uppercase().as_str() {
        "INT64" | "INT" | "BIGINT" => Some(ArrayAttrType::Int64),
        "FLOAT64" | "DOUBLE" | "FLOAT" => Some(ArrayAttrType::Float64),
        "STRING" | "TEXT" | "VARCHAR" => Some(ArrayAttrType::String),
        "BYTES" | "BLOB" | "BYTEA" => Some(ArrayAttrType::Bytes),
        _ => None,
    }
}
