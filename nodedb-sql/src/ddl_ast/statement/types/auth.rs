// SPDX-License-Identifier: Apache-2.0

//! Auth DDL/DML statements.

use crate::ddl_ast::alter_ops::{AlterRoleOp, AlterUserOp};
use crate::ddl_ast::statement::auth::OidcClaimMappingClause;

#[derive(Debug, Clone, PartialEq)]
pub enum AuthStmt {
    // ── User / auth / grant ──────────────────────────────────────
    CreateUser {
        username: String,
        password: String,
        role: Option<String>,
        tenant_id: Option<u64>,
    },
    DropUser {
        username: String,
    },
    AlterUser {
        username: String,
        op: AlterUserOp,
    },
    ShowUsers,
    /// `ALTER ROLE <name> GRANT/REVOKE/SET`.
    AlterRole {
        name: String,
        sub_op: AlterRoleOp,
    },
    GrantRole {
        role: String,
        username: String,
    },
    RevokeRole {
        role: String,
        username: String,
    },
    GrantPermission {
        permission: String,
        target_type: String,
        target_name: String,
        grantee: String,
    },
    /// `GRANT <privilege> ON DATABASE <name> TO <user>`
    GrantDatabasePermission {
        permission: String,
        db_name: String,
        grantee: String,
    },
    RevokePermission {
        permission: String,
        target_type: String,
        target_name: String,
        grantee: String,
    },
    /// `REVOKE <privilege> ON DATABASE <name> FROM <user>`
    RevokeDatabasePermission {
        permission: String,
        db_name: String,
        grantee: String,
    },
    ShowPermissions {
        on_collection: Option<String>,
        for_grantee: Option<String>,
    },
    ShowGrants {
        username: Option<String>,
    },

    // ── OIDC providers ───────────────────────────────────────────
    /// `CREATE OIDC PROVIDER <name> ISSUER '<iss>' JWKS_URI '<uri>'
    ///  [AUDIENCE '<aud>'] [CLAIM MAPPING WHEN <claim_name> = '<value>'
    ///  SET DEFAULT_DATABASE = <id>, ADD DATABASES [<ids>], ADD ROLES ['<role>', ...]]`
    CreateOidcProvider {
        name: String,
        issuer: String,
        jwks_uri: String,
        audience: Option<String>,
        /// `(claim_name, claim_value, default_database, add_databases, add_roles)` tuples.
        claim_mappings: Vec<OidcClaimMappingClause>,
    },
    /// `ALTER OIDC PROVIDER <name> SET CLAIM MAPPING WHEN <claim_name> = '<value>'
    ///  SET DEFAULT_DATABASE = <id>, ADD DATABASES [<ids>], ADD ROLES ['<role>', ...]`
    ///
    /// Replaces the entire claim-mapping list for the named provider.
    AlterOidcProviderClaimMapping {
        name: String,
        claim_mappings: Vec<OidcClaimMappingClause>,
    },
    /// `DROP OIDC PROVIDER [IF EXISTS] <name>`
    DropOidcProvider {
        name: String,
        if_exists: bool,
    },
    /// `SHOW OIDC PROVIDERS`
    ShowOidcProviders,
}
