// SPDX-License-Identifier: BUSL-1.1

//! Pure claim-mapping logic: maps JWT claims to NodeDB databases and roles.
//!
//! No I/O, no state. Called by `verify_bearer_token` after the token has been
//! cryptographically verified.

use crate::control::security::catalog::oidc_providers::StoredClaimMappingRule;
use crate::control::security::jwt::JwtClaims;

/// Result of applying claim-mapping rules to a verified JWT.
#[derive(Debug, Clone, Default)]
pub struct ClaimMappingResult {
    /// Default database ID for the session. `None` = no claim-mapping override;
    /// the caller falls back to the user's stored default.
    pub default_database: Option<u64>,
    /// Database IDs the session may access, in addition to the default.
    pub accessible_databases: Vec<u64>,
    /// Role names granted by the matching rules.
    pub roles: Vec<String>,
}

/// Apply `rules` to `claims` and return the merged `ClaimMappingResult`.
///
/// Rules are evaluated in order. The **first** matching rule that sets a
/// `default_database` wins for that field. `add_databases` and `add_roles`
/// accumulate across all matching rules.
///
/// `claim_value = "*"` matches any non-empty claim value.
pub fn apply_claim_mapping(
    claims: &JwtClaims,
    rules: &[StoredClaimMappingRule],
) -> ClaimMappingResult {
    let mut result = ClaimMappingResult::default();

    for rule in rules {
        // Resolve the actual claim value from the JWT payload.
        let actual_value: Option<String> = match rule.claim_name.as_str() {
            "sub" => Some(claims.sub.clone()),
            "iss" => Some(claims.iss.clone()),
            "aud" => Some(claims.aud.clone()),
            other => claims
                .extra
                .get(other)
                .and_then(|v| v.as_str().map(str::to_owned)),
        };

        let Some(val) = actual_value else {
            continue;
        };

        // Match: exact value or wildcard.
        let matches = if rule.claim_value == "*" {
            !val.is_empty()
        } else {
            val == rule.claim_value
        };

        if !matches {
            continue;
        }

        // First matching rule that sets a default_database wins.
        if result.default_database.is_none()
            && let Some(db_id) = rule.default_database
        {
            result.default_database = Some(db_id);
        }

        // Accumulate accessible databases.
        for &db_id in &rule.add_databases {
            if !result.accessible_databases.contains(&db_id) {
                result.accessible_databases.push(db_id);
            }
        }

        // Accumulate roles.
        for role in &rule.add_roles {
            if !result.roles.contains(role) {
                result.roles.push(role.clone());
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::security::catalog::oidc_providers::StoredClaimMappingRule;
    use crate::control::security::jwt::JwtClaims;

    fn claims_with_org(org: &str) -> JwtClaims {
        let mut extra = std::collections::HashMap::new();
        extra.insert("org_id".into(), serde_json::Value::String(org.to_owned()));
        JwtClaims {
            sub: "alice".into(),
            tenant_id: 1,
            roles: vec![],
            exp: 9_999_999_999,
            nbf: 0,
            iat: 0,
            iss: "https://idp.example.com".into(),
            aud: "nodedb".into(),
            user_id: 0,
            is_superuser: false,
            extra,
        }
    }

    #[test]
    fn exact_match_resolves_database_and_roles() {
        let rules = vec![StoredClaimMappingRule {
            claim_name: "org_id".into(),
            claim_value: "acme".into(),
            default_database: Some(42),
            add_databases: vec![43],
            add_roles: vec!["readwrite".into()],
        }];
        let res = apply_claim_mapping(&claims_with_org("acme"), &rules);
        assert_eq!(res.default_database, Some(42));
        assert_eq!(res.accessible_databases, vec![43]);
        assert_eq!(res.roles, vec!["readwrite"]);
    }

    #[test]
    fn unknown_value_no_match() {
        let rules = vec![StoredClaimMappingRule {
            claim_name: "org_id".into(),
            claim_value: "acme".into(),
            default_database: Some(42),
            add_databases: vec![],
            add_roles: vec![],
        }];
        let res = apply_claim_mapping(&claims_with_org("other"), &rules);
        assert!(res.default_database.is_none());
        assert!(res.roles.is_empty());
    }

    #[test]
    fn wildcard_matches_any_nonempty() {
        let rules = vec![StoredClaimMappingRule {
            claim_name: "org_id".into(),
            claim_value: "*".into(),
            default_database: Some(1),
            add_databases: vec![],
            add_roles: vec!["readonly".into()],
        }];
        let res = apply_claim_mapping(&claims_with_org("anything"), &rules);
        assert_eq!(res.default_database, Some(1));
        assert_eq!(res.roles, vec!["readonly"]);
    }

    #[test]
    fn wildcard_does_not_match_empty_value() {
        let rules = vec![StoredClaimMappingRule {
            claim_name: "org_id".into(),
            claim_value: "*".into(),
            default_database: Some(1),
            add_databases: vec![],
            add_roles: vec![],
        }];
        let res = apply_claim_mapping(&claims_with_org(""), &rules);
        assert!(res.default_database.is_none());
    }

    #[test]
    fn first_matching_rule_wins_default_db() {
        let rules = vec![
            StoredClaimMappingRule {
                claim_name: "org_id".into(),
                claim_value: "*".into(),
                default_database: Some(10),
                add_databases: vec![],
                add_roles: vec![],
            },
            StoredClaimMappingRule {
                claim_name: "org_id".into(),
                claim_value: "*".into(),
                default_database: Some(20),
                add_databases: vec![],
                add_roles: vec![],
            },
        ];
        let res = apply_claim_mapping(&claims_with_org("x"), &rules);
        // First rule wins for default_database.
        assert_eq!(res.default_database, Some(10));
    }

    #[test]
    fn roles_accumulate_across_rules() {
        let rules = vec![
            StoredClaimMappingRule {
                claim_name: "org_id".into(),
                claim_value: "*".into(),
                default_database: Some(1),
                add_databases: vec![],
                add_roles: vec!["r1".into()],
            },
            StoredClaimMappingRule {
                claim_name: "sub".into(),
                claim_value: "alice".into(),
                default_database: None,
                add_databases: vec![],
                add_roles: vec!["r2".into()],
            },
        ];
        let res = apply_claim_mapping(&claims_with_org("y"), &rules);
        assert!(res.roles.contains(&"r1".to_owned()));
        assert!(res.roles.contains(&"r2".to_owned()));
    }
}
