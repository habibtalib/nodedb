// SPDX-License-Identifier: BUSL-1.1

//! Pure-data types shared by enforcement logic across engines.
//!
//! These types carry no behavior — they are plain data structs that cross
//! the SPSC bridge as part of `EnforcementOptions`. Defined here so that
//! `DocumentOp` (and eventually `EnforcementOptions`) can migrate to this
//! shared crate without pulling in Origin-internal modules.

/// Parsed retention duration with calendar-accurate units.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct RetentionDuration {
    pub count: u32,
    pub unit: RetentionUnit,
}

/// Calendar-accurate duration units.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
#[msgpack(c_enum)]
pub enum RetentionUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
    Weeks,
    Months,
    Years,
}

/// State transition constraint: column value can only change along declared paths.
#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct StateTransitionDef {
    pub name: String,
    pub column: String,
    pub transitions: Vec<TransitionRule>,
}

/// A single allowed state transition, optionally guarded by a role.
#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct TransitionRule {
    pub from: String,
    pub to: String,
    pub required_role: Option<String>,
}

/// Transition check predicate: evaluated on UPDATE with OLD and NEW access.
#[derive(
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    zerompk::ToMessagePack,
    zerompk::FromMessagePack,
)]
pub struct TransitionCheckDef {
    pub name: String,
    pub predicate: nodedb_query::expr::SqlExpr,
}
