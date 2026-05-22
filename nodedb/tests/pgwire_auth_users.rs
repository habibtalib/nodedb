// SPDX-License-Identifier: BUSL-1.1

//! CREATE/DROP/ALTER USER over the pgwire DDL path, plus the two
//! readonly-permission guards that live on user-mgmt surfaces.

mod common;

use common::pgwire_auth_helpers::{
    assert_readonly_denied, ddl_err, ddl_ok, make_state, make_state_with_catalog, superuser,
};
use nodedb::control::security::credential::store::CredentialStore;
use nodedb::control::security::identity::Role;
use nodedb::types::TenantId;

#[tokio::test]
async fn create_user() {
    let state = make_state();
    let su = superuser();
    ddl_ok(
        &state,
        &su,
        "CREATE USER alice WITH PASSWORD 'secret123' ROLE readwrite TENANT 1",
    )
    .await;

    let user = state.credentials.get_user("alice").unwrap();
    assert_eq!(user.tenant_id, TenantId::new(1));
    assert!(user.roles.contains(&Role::ReadWrite));
    assert!(!user.is_superuser);
}

#[tokio::test]
async fn create_user_duplicate_rejected() {
    let state = make_state();
    let su = superuser();
    ddl_ok(&state, &su, "CREATE USER bob WITH PASSWORD 'pass'").await;

    let err = ddl_err(&state, &su, "CREATE USER bob WITH PASSWORD 'pass2'").await;
    assert!(
        err.contains("already exists"),
        "expected duplicate error: {err}"
    );
}

#[tokio::test]
async fn create_user_default_role_and_tenant() {
    let state = make_state();
    let su = superuser();
    ddl_ok(&state, &su, "CREATE USER carol WITH PASSWORD 'pass'").await;

    let user = state.credentials.get_user("carol").unwrap();
    // Default role is readwrite, tenant inherited from identity (0 for superuser).
    assert!(user.roles.contains(&Role::ReadWrite));
}

#[tokio::test]
async fn drop_user() {
    let state = make_state();
    let su = superuser();
    ddl_ok(&state, &su, "CREATE USER dave WITH PASSWORD 'pass'").await;
    ddl_ok(&state, &su, "DROP USER dave").await;

    assert!(state.credentials.get_user("dave").is_none());
}

#[tokio::test]
async fn drop_self_rejected() {
    let state = make_state();
    let su = superuser();
    let err = ddl_err(&state, &su, "DROP USER nodedb").await;
    assert!(err.contains("cannot drop your own"), "{err}");
}

#[tokio::test]
async fn drop_nonexistent_user() {
    let state = make_state();
    let su = superuser();
    let err = ddl_err(&state, &su, "DROP USER nobody").await;
    assert!(err.contains("does not exist"), "{err}");
}

#[tokio::test]
async fn alter_user_password() {
    let state = make_state();
    let su = superuser();
    ddl_ok(&state, &su, "CREATE USER eve WITH PASSWORD 'old'").await;
    ddl_ok(&state, &su, "ALTER USER eve SET PASSWORD 'new'").await;

    assert!(state.credentials.verify_password("eve", "new"));
    assert!(!state.credentials.verify_password("eve", "old"));
}

#[tokio::test]
async fn alter_user_role() {
    let state = make_state();
    let su = superuser();
    ddl_ok(
        &state,
        &su,
        "CREATE USER frank WITH PASSWORD 'pass' ROLE readonly",
    )
    .await;
    ddl_ok(&state, &su, "ALTER USER frank SET ROLE readwrite").await;

    let user = state.credentials.get_user("frank").unwrap();
    assert!(user.roles.contains(&Role::ReadWrite));
}

#[tokio::test]
async fn drop_then_recreate_same_name() {
    // DROP USER must fully free the username. Recreating a dropped
    // user with the same name (the routine "rotate credentials"
    // operation) must succeed — not fail with "already exists".
    let state = make_state();
    let su = superuser();
    ddl_ok(
        &state,
        &su,
        "CREATE USER demo2 WITH PASSWORD 'oldpass' ROLE readwrite TENANT 2",
    )
    .await;
    ddl_ok(&state, &su, "DROP USER demo2").await;
    assert!(state.credentials.get_user("demo2").is_none());

    ddl_ok(
        &state,
        &su,
        "CREATE USER demo2 WITH PASSWORD 'newpass' ROLE readwrite TENANT 2",
    )
    .await;

    // The recreated user must be a fresh, active record — not the
    // stale tombstone resurrected with its old credentials.
    let user = state
        .credentials
        .get_user("demo2")
        .expect("recreated user must be visible");
    assert!(user.is_active);
    assert!(
        state.credentials.verify_password("demo2", "newpass"),
        "recreated user must carry the new password"
    );
    assert!(
        !state.credentials.verify_password("demo2", "oldpass"),
        "stale credentials from the dropped user must not survive"
    );
}

#[test]
fn dropped_username_is_free_after_restart() {
    // The stale identity record must not survive a daemon restart.
    // A username dropped before restart must be reusable after the
    // catalog is reloaded from disk.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("system.redb");
    {
        let store = CredentialStore::open(&path).unwrap();
        store
            .create_user("demo2", "oldpass", TenantId::new(2), vec![Role::ReadWrite])
            .unwrap();
        assert!(store.drop_user("demo2").unwrap());
    }

    // Simulate the daemon restart: reopen the same on-disk catalog.
    let store = CredentialStore::open(&path).unwrap();
    store
        .create_user("demo2", "newpass", TenantId::new(2), vec![Role::ReadWrite])
        .expect("recreating a dropped user after restart must succeed");

    let user = store
        .get_user("demo2")
        .expect("recreated user must be visible after restart");
    assert!(user.is_active);
}

#[test]
fn dropped_username_is_free_for_service_account() {
    // The CREATE-time uniqueness check for service accounts shares
    // the user uniqueness store. A name freed by DROP USER must be
    // available to a new service account.
    let store = CredentialStore::new();
    store
        .create_user("demo2", "oldpass", TenantId::new(2), vec![Role::ReadWrite])
        .unwrap();
    assert!(store.drop_user("demo2").unwrap());

    store
        .create_service_account("demo2", TenantId::new(2), vec![Role::ReadWrite], vec![])
        .expect("a dropped user's name must be free for a service account");
}

#[tokio::test]
async fn readonly_cannot_create_user() {
    let state = make_state();
    assert_readonly_denied(&state, "CREATE USER hacker WITH PASSWORD 'x'").await;
}

#[tokio::test]
async fn readonly_cannot_drop_user() {
    let state = make_state();
    let su = superuser();
    ddl_ok(&state, &su, "CREATE USER target WITH PASSWORD 'pass'").await;

    assert_readonly_denied(&state, "DROP USER target").await;
}

/// `DROP USER IF EXISTS <name>` on a user that does not exist is a no-op
/// success, not an error.
#[tokio::test]
async fn drop_user_if_exists_missing_is_noop() {
    let state = make_state();
    let su = superuser();
    ddl_ok(&state, &su, "DROP USER IF EXISTS ghost").await;
}

/// `DROP USER IF EXISTS <name>` on an existing user actually drops it —
/// the `IF EXISTS` clause must not turn the statement into a total no-op.
#[tokio::test]
async fn drop_user_if_exists_existing_drops() {
    let state = make_state();
    let su = superuser();
    ddl_ok(&state, &su, "CREATE USER target WITH PASSWORD 'pass'").await;
    ddl_ok(&state, &su, "DROP USER IF EXISTS target").await;

    assert!(
        state.credentials.get_user("target").is_none(),
        "DROP USER IF EXISTS must drop an existing user"
    );
}

/// `CREATE USER ... TENANT '<name>'` resolves the tenant by name, so
/// admins are not forced to look up numeric ids from `SHOW TENANTS`.
#[tokio::test]
async fn create_user_tenant_by_name() {
    let state = make_state_with_catalog();
    let su = superuser();
    ddl_ok(&state, &su, "CREATE TENANT acme ID 42").await;
    ddl_ok(
        &state,
        &su,
        "CREATE USER alice WITH PASSWORD 'secret123' TENANT 'acme'",
    )
    .await;

    let user = state.credentials.get_user("alice").unwrap();
    assert_eq!(
        user.tenant_id,
        TenantId::new(42),
        "TENANT '<name>' must resolve to the named tenant's id"
    );
}
