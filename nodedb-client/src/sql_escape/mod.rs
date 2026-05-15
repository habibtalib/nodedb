// SPDX-License-Identifier: Apache-2.0

//! Shared SQL escaping helpers used by both the native and the remote
//! clients. One implementation per escape rule; call sites elsewhere
//! must not re-implement these.

pub(crate) mod identifier;
pub(crate) mod literal;

pub(crate) use identifier::quote_identifier;
pub(crate) use literal::quote_string_literal;
