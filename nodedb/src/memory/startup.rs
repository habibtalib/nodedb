// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use tracing::info;

use nodedb_mem::EngineId;
use nodedb_mem::governor::{GovernorConfig, MemoryGovernor};

use crate::config::engine::EngineByteBudgets;

/// Initialize the memory governor from engine byte budgets.
///
/// Called once at startup. The returned governor is shared (via `Arc`)
/// across the Control Plane and all Data Plane cores.
///
/// `budgets` carries a byte limit for **every** [`EngineId`]; the governor
/// reports any engine without a budget as being at Emergency pressure,
/// which rejects that engine's writes with `resources exhausted`, so the
/// full map is mandatory. [`EngineByteBudgets`] is built by
/// [`crate::config::EngineConfig::to_byte_budgets`], which derives one
/// entry per `EngineId::ALL` member.
pub fn init_governor(
    global_ceiling: usize,
    budgets: &EngineByteBudgets,
) -> crate::Result<Arc<MemoryGovernor>> {
    let engine_limits = budgets.as_engine_limits().clone();

    let config = GovernorConfig {
        global_ceiling,
        engine_limits,
    };

    let governor = MemoryGovernor::new(config).map_err(|e| crate::Error::Config {
        detail: format!("failed to initialize memory governor: {e}"),
    })?;

    info!(
        global_ceiling,
        engines = EngineId::ALL.len(),
        total_engine_budget = budgets.total(),
        "memory governor initialized"
    );

    Ok(Arc::new(governor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::engine::EngineConfig;

    #[test]
    fn init_from_default_config() {
        let cfg = EngineConfig::default();
        let budgets = cfg.to_byte_budgets(1024 * 1024 * 1024); // 1 GiB
        let gov = init_governor(1024 * 1024 * 1024, &budgets).unwrap();

        assert!(gov.budget(EngineId::Vector).is_some());
        assert!(gov.budget(EngineId::Query).is_some());
        assert!(gov.budget(EngineId::Crdt).is_some());
    }

    #[test]
    fn init_rejects_impossible_budgets() {
        // Byte budgets derived against a 1 MiB ceiling sum to ~1 MiB; feeding
        // them to a governor whose ceiling is only 1000 bytes must fail
        // `GovernorConfig::validate`.
        let budgets = EngineConfig::default().to_byte_budgets(1024 * 1024);
        assert!(budgets.total() > 1000);
        let result = init_governor(1000, &budgets);
        assert!(result.is_err());
    }

    /// Build the governor exactly as the server does on a fresh boot:
    /// default engine fractions of a 1 GiB ceiling.
    fn fresh_boot_governor() -> Arc<MemoryGovernor> {
        let global = 1024 * 1024 * 1024usize;
        let budgets = EngineConfig::default().to_byte_budgets(global);
        init_governor(global, &budgets).expect("default config must produce a governor")
    }

    /// `MemoryGovernor::engine_pressure` returns `PressureLevel::Emergency`
    /// for any engine that has no budget registered. The Data Plane calls
    /// `check_engine_pressure` at the top of every write handler, so an
    /// engine missing from the governor's map turns the very first write to
    /// that engine on a fresh server into a client-facing `resources
    /// exhausted` error — even though the box has gigabytes free.
    ///
    /// This is the reported `document_schemaless` symptom; the other
    /// assertions below cover the sibling engines that share the same
    /// write-pressure gate.
    #[test]
    fn document_schemaless_writes_not_starved_on_fresh_governor() {
        let gov = fresh_boot_governor();
        assert!(
            gov.budget(EngineId::DocumentSchemaless).is_some(),
            "document_schemaless has no memory budget on a fresh server"
        );
        assert_ne!(
            gov.engine_pressure(EngineId::DocumentSchemaless),
            nodedb_mem::PressureLevel::Emergency,
            "document_schemaless reports Emergency pressure on a fresh, empty governor — \
             first INSERT will fail with `resources exhausted`"
        );
    }

    #[test]
    fn kv_writes_not_starved_on_fresh_governor() {
        let gov = fresh_boot_governor();
        assert!(
            gov.budget(EngineId::Kv).is_some(),
            "kv has no memory budget"
        );
        assert_ne!(
            gov.engine_pressure(EngineId::Kv),
            nodedb_mem::PressureLevel::Emergency,
            "kv reports Emergency pressure on a fresh governor"
        );
    }

    #[test]
    fn columnar_writes_not_starved_on_fresh_governor() {
        let gov = fresh_boot_governor();
        assert!(
            gov.budget(EngineId::Columnar).is_some(),
            "columnar has no memory budget"
        );
        assert_ne!(
            gov.engine_pressure(EngineId::Columnar),
            nodedb_mem::PressureLevel::Emergency,
            "columnar reports Emergency pressure on a fresh governor"
        );
    }

    #[test]
    fn array_writes_not_starved_on_fresh_governor() {
        let gov = fresh_boot_governor();
        assert!(
            gov.budget(EngineId::Array).is_some(),
            "array has no memory budget"
        );
        assert_ne!(
            gov.engine_pressure(EngineId::Array),
            nodedb_mem::PressureLevel::Emergency,
            "array reports Emergency pressure on a fresh governor"
        );
    }

    #[test]
    fn graph_writes_not_starved_on_fresh_governor() {
        let gov = fresh_boot_governor();
        assert!(
            gov.budget(EngineId::Graph).is_some(),
            "graph has no memory budget"
        );
        assert_ne!(
            gov.engine_pressure(EngineId::Graph),
            nodedb_mem::PressureLevel::Emergency,
            "graph reports Emergency pressure on a fresh governor"
        );
    }

    #[test]
    fn fts_indexing_not_starved_on_fresh_governor() {
        // Every document write also runs `check_engine_pressure(EngineId::Fts)`
        // because FTS indexing is a side effect of the write.
        let gov = fresh_boot_governor();
        assert!(
            gov.budget(EngineId::Fts).is_some(),
            "fts has no memory budget"
        );
        assert_ne!(
            gov.engine_pressure(EngineId::Fts),
            nodedb_mem::PressureLevel::Emergency,
            "fts reports Emergency pressure on a fresh governor"
        );
    }

    /// The root invariant: every engine identifier the rest of the system
    /// can name must have a budget after `init_governor`. `engine_pressure`
    /// fail-closes to `Emergency` for unknown engines and `try_reserve`
    /// returns `UnknownEngine`, so a partial registration silently breaks
    /// every code path keyed on the missing engine.
    #[test]
    fn every_engine_has_a_budget_on_fresh_governor() {
        let gov = fresh_boot_governor();
        let missing: Vec<_> = EngineId::ALL
            .iter()
            .filter(|e| gov.budget(**e).is_none())
            .map(|e| e.to_string())
            .collect();
        assert!(
            missing.is_empty(),
            "engines with no memory budget after init_governor: {missing:?}"
        );
        for &engine in EngineId::ALL {
            assert_ne!(
                gov.engine_pressure(engine),
                nodedb_mem::PressureLevel::Emergency,
                "{engine} reports Emergency pressure on a fresh governor"
            );
        }
    }
}
