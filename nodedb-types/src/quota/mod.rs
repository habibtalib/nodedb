// SPDX-License-Identifier: Apache-2.0

//! Database and tenant resource quota types.

pub mod priority_class;
pub mod quota_record;

pub use priority_class::{PriorityClass, PriorityClassParseError};
pub use quota_record::{QuotaRecord, QuotaSpec, QuotaValidationError};
