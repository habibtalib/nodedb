// SPDX-License-Identifier: BUSL-1.1

//! Small parsing helpers used across pgwire DDL handlers: role-name parsing
//! and lenient hex decoding for binary literals.

use crate::control::security::identity::Role;

/// Parse a role name string into a `Role`.
///
/// Known roles map to their enum variants; unknown names become `Role::Custom`.
pub fn parse_role(name: &str) -> Role {
    // Role::from_str is Infallible — unwrap is safe on Infallible.
    match name.parse() {
        Ok(role) => role,
        Err(e) => match e {},
    }
}

/// Decode a hex string into bytes.
///
/// Returns `None` if the input has an odd number of characters or contains
/// characters that are not valid hexadecimal digits.
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16).ok()?;
        bytes.push(byte);
    }
    Some(bytes)
}
