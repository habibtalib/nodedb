// SPDX-License-Identifier: BUSL-1.1

//! pgwire handlers for OIDC provider DDL.
//!
//! - `CREATE OIDC PROVIDER <name> ISSUER '<iss>' JWKS_URI '<uri>' ...`
//! - `ALTER OIDC PROVIDER <name> SET CLAIM MAPPING WHEN ...`
//! - `DROP OIDC PROVIDER [IF EXISTS] <name>`
//! - `SHOW OIDC PROVIDERS`
//!
//! OIDC providers are system-scoped (superuser-only) and backed by the
//! `_system.oidc_providers` catalog table.

mod alter;
mod create;
mod drop;
mod show;

pub use alter::alter_oidc_provider_claim_mapping;
pub use create::create_oidc_provider;
pub use drop::drop_oidc_provider;
pub use show::show_oidc_providers;
