//! Node and edge binding parsers for Cypher MATCH patterns.

use super::super::ast::*;

/// Parse a node binding: `(a:Person)`, `(:Person)`, `(a)`, `()`.
pub(super) fn parse_node_binding(text: &str) -> crate::Result<(NodeBinding, usize)> {
    let trimmed = text.trim_start();
    let offset = text.len() - trimmed.len();

    if !trimmed.starts_with('(') {
        return Err(crate::Error::BadRequest {
            detail: format!(
                "expected '(' for node binding, got: '{}'",
                &trimmed[..trimmed.len().min(20)]
            ),
        });
    }

    let close = trimmed.find(')').ok_or(crate::Error::BadRequest {
        detail: "unclosed '(' in node binding".to_string(),
    })?;
    let inner = trimmed[1..close].trim();

    // Strip any `{...}` property filter block from inner before parsing name/label.
    let inner_stripped: std::borrow::Cow<str> = if let Some(brace_start) = inner.find('{') {
        let before_brace = inner[..brace_start].trim();
        std::borrow::Cow::Owned(before_brace.to_string())
    } else {
        std::borrow::Cow::Borrowed(inner)
    };
    let inner = inner_stripped.as_ref();

    let (name, label) = if inner.is_empty() {
        (None, None)
    } else if let Some(colon_pos) = inner.find(':') {
        let name_part = inner[..colon_pos].trim();
        let label_part = inner[colon_pos + 1..].trim();
        (
            if name_part.is_empty() {
                None
            } else {
                Some(name_part.to_string())
            },
            if label_part.is_empty() {
                None
            } else {
                Some(label_part.to_string())
            },
        )
    } else {
        (Some(inner.to_string()), None)
    };

    Ok((NodeBinding { name, label }, offset + close + 1))
}

/// Parse an edge binding: `-[:KNOWS]->`, `<-[:KNOWS]-`, `-[r:KNOWS*1..3]->`, `-[]-`, etc.
pub(super) fn parse_edge_binding(text: &str) -> crate::Result<(EdgeBinding, usize)> {
    let trimmed = text.trim_start();
    let offset = text.len() - trimmed.len();

    // Detect direction prefix.
    let (left_arrow, after_prefix) = if let Some(after) = trimmed.strip_prefix("<-") {
        (true, after)
    } else if let Some(after) = trimmed.strip_prefix('-') {
        (false, after)
    } else {
        return Err(crate::Error::BadRequest {
            detail: format!(
                "expected '-' or '<-' for edge, got: '{}'",
                &trimmed[..trimmed.len().min(20)]
            ),
        });
    };

    // Parse bracket content.
    let (name, edge_type, min_hops, max_hops, after_bracket, bracket_len) =
        if after_prefix.starts_with('[') {
            let close = after_prefix.find(']').ok_or(crate::Error::BadRequest {
                detail: "unclosed '[' in edge binding".to_string(),
            })?;
            let inner = after_prefix[1..close].trim();
            let (n, t, mi, ma) = parse_edge_inner(inner)?;
            (n, t, mi, ma, &after_prefix[close + 1..], close + 1)
        } else {
            (None, None, 1, 1, after_prefix, 0)
        };

    // Detect direction suffix.
    let (right_arrow, consumed_suffix) = if after_bracket.starts_with("->") {
        (true, 2)
    } else if after_bracket.starts_with('-') {
        (false, 1)
    } else {
        return Err(crate::Error::BadRequest {
            detail: format!(
                "expected '->' or '-' after edge, got: '{}'",
                &after_bracket[..after_bracket.len().min(20)]
            ),
        });
    };

    let direction = match (left_arrow, right_arrow) {
        (false, true) => EdgeDirection::Right,
        (true, false) => EdgeDirection::Left,
        (false, false) => EdgeDirection::Both,
        (true, true) => {
            return Err(crate::Error::BadRequest {
                detail: "<-[]-> is not valid; use -[]- for undirected".to_string(),
            });
        }
    };

    let total_consumed = offset + (if left_arrow { 2 } else { 1 }) + bracket_len + consumed_suffix;

    Ok((
        EdgeBinding {
            name,
            edge_type,
            direction,
            min_hops,
            max_hops,
        },
        total_consumed,
    ))
}

/// Parse the inner content of an edge bracket: `r:KNOWS*1..3`, `:KNOWS`, `r`, `*2..5`.
fn parse_edge_inner(inner: &str) -> crate::Result<(Option<String>, Option<String>, usize, usize)> {
    if inner.is_empty() {
        return Ok((None, None, 1, 1));
    }

    let (main_part, hops_part) = if let Some(star_pos) = inner.find('*') {
        (&inner[..star_pos], Some(&inner[star_pos + 1..]))
    } else {
        (inner, None)
    };

    let (name, edge_type) = if let Some(colon_pos) = main_part.find(':') {
        let name_part = main_part[..colon_pos].trim();
        let type_part = main_part[colon_pos + 1..].trim();
        (
            if name_part.is_empty() {
                None
            } else {
                Some(name_part.to_string())
            },
            if type_part.is_empty() {
                None
            } else {
                Some(type_part.to_string())
            },
        )
    } else {
        let trimmed = main_part.trim();
        if trimmed.is_empty() {
            (None, None)
        } else {
            (Some(trimmed.to_string()), None)
        }
    };

    let (min_hops, max_hops) = match hops_part {
        None => (1, 1),
        Some(hops) => parse_hop_range(hops.trim())?,
    };

    Ok((name, edge_type, min_hops, max_hops))
}

/// Parse hop range: `1..3`, `2`, `..5`, `1..`.
pub(super) fn parse_hop_range(s: &str) -> crate::Result<(usize, usize)> {
    if s.is_empty() {
        return Ok((1, 1));
    }

    if let Some(dots) = s.find("..") {
        let min_str = s[..dots].trim();
        let max_str = s[dots + 2..].trim();

        let min = if min_str.is_empty() {
            1
        } else {
            min_str.parse().map_err(|_| crate::Error::BadRequest {
                detail: format!("invalid min hops: '{min_str}'"),
            })?
        };

        let max = if max_str.is_empty() {
            10
        } else {
            max_str.parse().map_err(|_| crate::Error::BadRequest {
                detail: format!("invalid max hops: '{max_str}'"),
            })?
        };

        if min > max {
            return Err(crate::Error::BadRequest {
                detail: format!("min hops ({min}) > max hops ({max})"),
            });
        }

        Ok((min, max))
    } else {
        let n: usize = s.parse().map_err(|_| crate::Error::BadRequest {
            detail: format!("invalid hop count: '{s}'"),
        })?;
        Ok((n, n))
    }
}
