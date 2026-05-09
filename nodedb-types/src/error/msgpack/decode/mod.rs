// SPDX-License-Identifier: Apache-2.0

//! MsgPack decoding for [`crate::error::details::ErrorDetails`].
//!
//! Split into:
//! - [`readers`]: small typed field-reader helpers shared across variants.
//! - [`from_messagepack`]: the `FromMessagePack` impl that dispatches on
//!   the tag byte to the appropriate reader and constructs the variant.

mod from_messagepack;
mod readers;
