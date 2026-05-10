// SPDX-License-Identifier: BUSL-1.1

//! Synchronous catalog application for `PutOidcProvider` / `DeleteOidcProvider`.

use crate::control::security::catalog::{StoredOidcProvider, SystemCatalog};

pub fn put(provider: &StoredOidcProvider, catalog: &SystemCatalog) {
    if let Err(e) = catalog.put_oidc_provider(provider) {
        tracing::error!(provider = %provider.provider_name, error = %e, "put_oidc_provider failed");
    }
}

pub fn delete(name: &str, catalog: &SystemCatalog) {
    if let Err(e) = catalog.delete_oidc_provider(name) {
        tracing::error!(provider = %name, error = %e, "delete_oidc_provider failed");
    }
}
