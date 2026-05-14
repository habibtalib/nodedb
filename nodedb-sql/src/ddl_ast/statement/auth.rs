// SPDX-License-Identifier: Apache-2.0

/// One `WHEN <claim_name> = '<value>' SET ...` clause inside a
/// `CREATE OIDC PROVIDER` or `ALTER OIDC PROVIDER` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcClaimMappingClause {
    pub claim_name: String,
    pub claim_value: String,
    /// Optional default database ID to grant for matching tokens.
    pub default_database: Option<u64>,
    /// Additional database IDs accessible to matching tokens.
    pub add_databases: Vec<u64>,
    /// Role names to grant to matching tokens.
    pub add_roles: Vec<String>,
}
