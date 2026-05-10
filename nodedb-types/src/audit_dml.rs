// SPDX-License-Identifier: Apache-2.0

//! `AuditDmlMode` — per-database DML audit level.
//!
//! Controls which DML operations are recorded in the audit log for a given
//! database. Set via `ALTER DATABASE <name> SET AUDIT_DML = <mode>`.

/// DML audit mode for a database.
///
/// Every match on this enum must be exhaustive — no `_ =>` arms anywhere.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
#[msgpack(c_enum)]
pub enum AuditDmlMode {
    /// No DML auditing (default).
    #[default]
    None = 0,
    /// Audit write operations: INSERT, UPDATE, DELETE, BulkInsert, BulkDelete.
    Writes = 1,
    /// Audit all DML operations (reads + writes).
    ///
    /// Currently equivalent to `Writes` — read events do not flow through the
    /// Event Plane yet. Tracked separately.
    All = 2,
}

impl std::str::FromStr for AuditDmlMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "NONE" => Ok(Self::None),
            "WRITES" => Ok(Self::Writes),
            "ALL" => Ok(Self::All),
            other => Err(format!(
                "unknown AUDIT_DML mode '{other}'; expected NONE, WRITES, or ALL"
            )),
        }
    }
}

impl std::fmt::Display for AuditDmlMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "NONE"),
            Self::Writes => write!(f, "WRITES"),
            Self::All => write!(f, "ALL"),
        }
    }
}
