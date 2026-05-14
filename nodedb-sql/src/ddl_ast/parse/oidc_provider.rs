// SPDX-License-Identifier: Apache-2.0

//! Parse OIDC provider DDL statements.
//!
//! Syntax:
//!
//! ```sql
//! CREATE OIDC PROVIDER <name>
//!     ISSUER '<iss>'
//!     JWKS_URI '<uri>'
//!     [AUDIENCE '<aud>']
//!     [CLAIM MAPPING WHEN <claim_name> = '<value>'
//!         [SET DEFAULT_DATABASE = <id>]
//!         [ADD DATABASES [<id>, ...]]
//!         [ADD ROLES ['<role>', ...]]
//!     ...]
//!
//! ALTER OIDC PROVIDER <name>
//!     SET CLAIM MAPPING WHEN <claim_name> = '<value>'
//!         [SET DEFAULT_DATABASE = <id>]
//!         [ADD DATABASES [<id>, ...]]
//!         [ADD ROLES ['<role>', ...]]
//!     [...]
//!
//! DROP OIDC PROVIDER [IF EXISTS] <name>
//!
//! SHOW OIDC PROVIDERS
//! ```

use crate::ddl_ast::statement::{AuthStmt, NodedbStatement, OidcClaimMappingClause};
use crate::error::SqlError;

pub(super) fn try_parse(
    upper: &str,
    parts: &[&str],
    trimmed: &str,
) -> Option<Result<NodedbStatement, SqlError>> {
    if upper.starts_with("CREATE OIDC PROVIDER ") {
        return Some(parse_create(parts, trimmed));
    }
    if upper.starts_with("ALTER OIDC PROVIDER ") {
        return Some(parse_alter(parts, trimmed));
    }
    if upper.starts_with("DROP OIDC PROVIDER ") {
        return Some(parse_drop(parts));
    }
    if upper == "SHOW OIDC PROVIDERS" {
        return Some(Ok(NodedbStatement::Auth(AuthStmt::ShowOidcProviders)));
    }
    None
}

// ── CREATE ─────────────────────────────────────────────────────────────────

fn parse_create(parts: &[&str], trimmed: &str) -> Result<NodedbStatement, SqlError> {
    // parts: CREATE OIDC PROVIDER <name> ISSUER '<iss>' JWKS_URI '<uri>' ...
    let name = parts
        .get(3)
        .ok_or_else(|| SqlError::Parse {
            detail: "syntax: CREATE OIDC PROVIDER <name> ISSUER '<iss>' JWKS_URI '<uri>'"
                .to_string(),
        })?
        .to_string();

    let issuer = extract_keyword_value(trimmed, "ISSUER").ok_or_else(|| SqlError::Parse {
        detail: "CREATE OIDC PROVIDER: ISSUER '<url>' is required".to_string(),
    })?;

    let jwks_uri = extract_keyword_value(trimmed, "JWKS_URI").ok_or_else(|| SqlError::Parse {
        detail: "CREATE OIDC PROVIDER: JWKS_URI '<url>' is required".to_string(),
    })?;

    let audience = extract_keyword_value(trimmed, "AUDIENCE");

    let claim_mappings = parse_claim_mappings(trimmed)?;

    Ok(NodedbStatement::Auth(AuthStmt::CreateOidcProvider {
        name,
        issuer,
        jwks_uri,
        audience,
        claim_mappings,
    }))
}

// ── ALTER ──────────────────────────────────────────────────────────────────

fn parse_alter(parts: &[&str], trimmed: &str) -> Result<NodedbStatement, SqlError> {
    // ALTER OIDC PROVIDER <name> SET CLAIM MAPPING WHEN ...
    let name = parts
        .get(3)
        .ok_or_else(|| SqlError::Parse {
            detail: "syntax: ALTER OIDC PROVIDER <name> SET CLAIM MAPPING WHEN ...".to_string(),
        })?
        .to_string();

    let claim_mappings = parse_claim_mappings(trimmed)?;

    Ok(NodedbStatement::Auth(
        AuthStmt::AlterOidcProviderClaimMapping {
            name,
            claim_mappings,
        },
    ))
}

// ── DROP ───────────────────────────────────────────────────────────────────

fn parse_drop(parts: &[&str]) -> Result<NodedbStatement, SqlError> {
    // DROP OIDC PROVIDER [IF EXISTS] <name>
    // parts: DROP OIDC PROVIDER ...
    let (if_exists, name_idx) = if parts.get(3).map(|s| s.eq_ignore_ascii_case("IF")) == Some(true)
    {
        (true, 5) // IF EXISTS <name>
    } else {
        (false, 3)
    };

    let name = parts
        .get(name_idx)
        .ok_or_else(|| SqlError::Parse {
            detail: "syntax: DROP OIDC PROVIDER [IF EXISTS] <name>".to_string(),
        })?
        .to_string();

    Ok(NodedbStatement::Auth(AuthStmt::DropOidcProvider {
        name,
        if_exists,
    }))
}

// ── Claim-mapping parser ────────────────────────────────────────────────────

/// Parse zero or more `WHEN <claim_name> = '<value>' ...` clauses.
///
/// Each clause is delimited by the next `WHEN` (case-insensitive) or end-of-input.
fn parse_claim_mappings(trimmed: &str) -> Result<Vec<OidcClaimMappingClause>, SqlError> {
    // Find all WHEN keyword positions (case-insensitive, whole-word).
    let upper = trimmed.to_uppercase();
    let mut clauses = Vec::new();

    // Split the input into segments starting at each "WHEN".
    let mut positions: Vec<usize> = Vec::new();
    let mut search_from = 0;
    while let Some(pos) = find_keyword(&upper, "WHEN", search_from) {
        positions.push(pos);
        search_from = pos + 4;
    }

    for (i, &start) in positions.iter().enumerate() {
        let end = positions.get(i + 1).copied().unwrap_or(trimmed.len());
        let segment = &trimmed[start..end];
        let clause = parse_when_clause(segment)?;
        clauses.push(clause);
    }

    Ok(clauses)
}

/// Parse a single `WHEN <claim_name> = '<value>' [SET DEFAULT_DATABASE = <id>]
/// [ADD DATABASES [...]] [ADD ROLES [...]]` segment.
fn parse_when_clause(segment: &str) -> Result<OidcClaimMappingClause, SqlError> {
    let parts: Vec<&str> = segment.split_whitespace().collect();
    // parts[0] = WHEN, parts[1] = <claim_name>, parts[2] = =, parts[3] = '<value>'
    let claim_name = parts
        .get(1)
        .ok_or_else(|| SqlError::Parse {
            detail: "CLAIM MAPPING: missing claim name after WHEN".to_string(),
        })?
        .to_lowercase();

    let raw_value = parts.get(3).ok_or_else(|| SqlError::Parse {
        detail: "CLAIM MAPPING: syntax is WHEN <claim> = '<value>'".to_string(),
    })?;
    let claim_value = raw_value.trim_matches('\'').trim_matches('"').to_string();

    let seg_upper = segment.to_uppercase();

    // SET DEFAULT_DATABASE = <id>
    let default_database = extract_u64_after_eq(&seg_upper, segment, "DEFAULT_DATABASE");

    // ADD DATABASES [<id>, ...]
    let add_databases = extract_u64_list(&seg_upper, segment, "DATABASES")?;

    // ADD ROLES ['<role>', ...]
    let add_roles = extract_string_list(segment, "ROLES")?;

    Ok(OidcClaimMappingClause {
        claim_name,
        claim_value,
        default_database,
        add_databases,
        add_roles,
    })
}

// ── Token extraction helpers ────────────────────────────────────────────────

/// Extract the quoted string value that follows `KEYWORD` in the SQL text.
///
/// Handles both single-quote and double-quote delimiters.
/// E.g. `ISSUER 'https://...'` → `"https://..."`.
fn extract_keyword_value(trimmed: &str, keyword: &str) -> Option<String> {
    let upper = trimmed.to_uppercase();
    let kw_upper = keyword.to_uppercase();
    let pos = upper.find(&kw_upper)?;
    let after = &trimmed[pos + kw_upper.len()..].trim_start();
    // The value is either 'quoted' or "double-quoted" or bare token.
    let tok = after.split_whitespace().next()?;
    let val = tok.trim_matches('\'').trim_matches('"').to_string();
    if val.is_empty() { None } else { Some(val) }
}

/// Extract a `u64` value following `KEYWORD = <id>`.
fn extract_u64_after_eq(seg_upper: &str, original: &str, keyword: &str) -> Option<u64> {
    let pos = seg_upper.find(keyword)?;
    let after_kw = &seg_upper[pos + keyword.len()..].trim_start();
    if !after_kw.starts_with('=') {
        return None;
    }
    let after_eq = &original[pos + keyword.len()..];
    let tok = after_eq
        .trim_start()
        .trim_start_matches('=')
        .split_whitespace()
        .next()?;
    tok.trim().parse::<u64>().ok()
}

/// Extract `[<id>, <id>, ...]` following `KEYWORD` (e.g. `DATABASES [1, 2, 3]`).
fn extract_u64_list(seg_upper: &str, original: &str, keyword: &str) -> Result<Vec<u64>, SqlError> {
    let pos = match seg_upper.find(keyword) {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let after = &original[pos + keyword.len()..].trim_start();
    if !after.starts_with('[') {
        // Possibly `ADD DATABASES` without a list — treat as empty.
        return Ok(Vec::new());
    }
    let close = after.find(']').ok_or_else(|| SqlError::Parse {
        detail: format!("ADD {keyword}: missing closing ']'"),
    })?;
    let inner = &after[1..close];
    inner
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u64>().map_err(|_| SqlError::Parse {
                detail: format!("ADD {keyword}: invalid database id '{s}'"),
            })
        })
        .collect()
}

/// Extract `['<role>', ...]` following `KEYWORD` (e.g. `ROLES ['admin', 'reader']`).
fn extract_string_list(original: &str, keyword: &str) -> Result<Vec<String>, SqlError> {
    let upper = original.to_uppercase();
    let pos = match upper.find(keyword) {
        Some(p) => p,
        None => return Ok(Vec::new()),
    };
    let after = &original[pos + keyword.len()..].trim_start();
    if !after.starts_with('[') {
        return Ok(Vec::new());
    }
    let close = after.find(']').ok_or_else(|| SqlError::Parse {
        detail: format!("ADD {keyword}: missing closing ']'"),
    })?;
    let inner = &after[1..close];
    Ok(inner
        .split(',')
        .map(|s| s.trim().trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// Find the byte offset of a whole-word keyword match (case-insensitive via
/// pre-uppercased `haystack`). The keyword must be uppercase.
fn find_keyword(haystack: &str, keyword: &str, from: usize) -> Option<usize> {
    let haystack = &haystack[from..];
    let klen = keyword.len();
    let mut search = 0;
    while let Some(pos) = haystack[search..].find(keyword) {
        let abs = search + pos;
        // Check word boundary: character before must not be alpha/digit.
        let before_ok = abs == 0
            || !haystack
                .as_bytes()
                .get(abs - 1)
                .copied()
                .map(|b| b.is_ascii_alphanumeric() || b == b'_')
                .unwrap_or(false);
        let after_ok = haystack
            .as_bytes()
            .get(abs + klen)
            .map(|&b| !b.is_ascii_alphanumeric() && b != b'_')
            .unwrap_or(true);
        if before_ok && after_ok {
            return Some(from + abs);
        }
        search = abs + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddl_ast::statement::NodedbStatement;

    fn ok(sql: &str) -> NodedbStatement {
        let upper = sql.to_uppercase();
        let parts: Vec<&str> = sql.split_whitespace().collect();
        try_parse(&upper, &parts, sql)
            .expect("expected Some")
            .expect("expected Ok")
    }

    #[test]
    fn show_oidc_providers() {
        assert_eq!(
            ok("SHOW OIDC PROVIDERS"),
            NodedbStatement::Auth(AuthStmt::ShowOidcProviders)
        );
    }

    #[test]
    fn drop_oidc_provider() {
        let stmt = ok("DROP OIDC PROVIDER myidp");
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::DropOidcProvider {
                name: "myidp".to_string(),
                if_exists: false,
            })
        );
    }

    #[test]
    fn drop_oidc_provider_if_exists() {
        let stmt = ok("DROP OIDC PROVIDER IF EXISTS myidp");
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::DropOidcProvider {
                name: "myidp".to_string(),
                if_exists: true,
            })
        );
    }

    #[test]
    fn create_oidc_provider_minimal() {
        let sql = "CREATE OIDC PROVIDER auth0 ISSUER 'https://auth0.example.com' JWKS_URI 'https://auth0.example.com/.well-known/jwks.json'";
        let stmt = ok(sql);
        assert_eq!(
            stmt,
            NodedbStatement::Auth(AuthStmt::CreateOidcProvider {
                name: "auth0".to_string(),
                issuer: "https://auth0.example.com".to_string(),
                jwks_uri: "https://auth0.example.com/.well-known/jwks.json".to_string(),
                audience: None,
                claim_mappings: vec![],
            })
        );
    }

    #[test]
    fn create_oidc_provider_with_audience() {
        let sql = "CREATE OIDC PROVIDER auth0 ISSUER 'https://idp.example.com' JWKS_URI 'https://idp.example.com/jwks' AUDIENCE 'nodedb-api'";
        let stmt = ok(sql);
        match stmt {
            NodedbStatement::Auth(AuthStmt::CreateOidcProvider { audience, .. }) => {
                assert_eq!(audience, Some("nodedb-api".to_string()));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn create_oidc_provider_with_claim_mapping() {
        let sql = "CREATE OIDC PROVIDER corp ISSUER 'https://sso.corp.com' JWKS_URI 'https://sso.corp.com/jwks' \
             AUDIENCE 'nodedb' \
             CLAIM MAPPING WHEN org_id = 'acme' SET DEFAULT_DATABASE = 42 ADD DATABASES [43, 44] ADD ROLES ['readwrite']";
        let stmt = ok(sql);
        match stmt {
            NodedbStatement::Auth(AuthStmt::CreateOidcProvider {
                name,
                claim_mappings,
                ..
            }) => {
                assert_eq!(name, "corp");
                assert_eq!(claim_mappings.len(), 1);
                let cm = &claim_mappings[0];
                assert_eq!(cm.claim_name, "org_id");
                assert_eq!(cm.claim_value, "acme");
                assert_eq!(cm.default_database, Some(42));
                assert_eq!(cm.add_databases, vec![43, 44]);
                assert_eq!(cm.add_roles, vec!["readwrite"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn alter_oidc_provider_claim_mapping() {
        let sql = "ALTER OIDC PROVIDER auth0 SET CLAIM MAPPING WHEN sub = '*' ADD ROLES ['admin']";
        let stmt = ok(sql);
        match stmt {
            NodedbStatement::Auth(AuthStmt::AlterOidcProviderClaimMapping {
                name,
                claim_mappings,
            }) => {
                assert_eq!(name, "auth0");
                assert_eq!(claim_mappings.len(), 1);
                assert_eq!(claim_mappings[0].claim_value, "*");
                assert_eq!(claim_mappings[0].add_roles, vec!["admin"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
