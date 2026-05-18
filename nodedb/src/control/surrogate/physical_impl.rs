// SPDX-License-Identifier: BUSL-1.1

//! Wires Origin's concrete [`SurrogateAssigner`] into the
//! `nodedb_physical::SurrogateAssigner` trait so the shared converter
//! can allocate surrogates without depending on Origin internals.

use nodedb_physical::{SurrogateAssignError, SurrogateAssigner as PhysicalSurrogateAssigner};

use super::assign::SurrogateAssigner;

impl PhysicalSurrogateAssigner for SurrogateAssigner {
    fn current_hwm(&self) -> u32 {
        Self::current_hwm(self)
    }

    fn assign(
        &self,
        collection: &str,
        pk_bytes: &[u8],
    ) -> Result<nodedb_types::Surrogate, SurrogateAssignError> {
        Self::assign(self, collection, pk_bytes)
            .map_err(|e| SurrogateAssignError::Backend(e.to_string()))
    }
}
