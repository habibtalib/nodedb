// SPDX-License-Identifier: BUSL-1.1

pub mod persist;
pub mod registry;

pub use persist::{DatabaseHwmPersist, SystemCatalogDatabaseHwm};
pub use registry::{
    DatabaseAllocError, DatabaseRegistry, FLUSH_ELAPSED_THRESHOLD, FLUSH_OPS_THRESHOLD,
    USER_DB_START,
};
