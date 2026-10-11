//! Unified output payload module for AutoResolve.
//! Standardizes the plan serialization format between AI multi-agent runs
//! and the deterministic AST engine.

use anyhow::Result;
use std::path::Path;

pub use crate::fix::{Edit, Plan, PlanItem};

/// Creates a new Plan initialized with the given target file and items.
pub fn create_plan(target_file: &str, items: Vec<PlanItem>) -> Plan {
    Plan {
        target_file: Some(target_file.to_string()),
        items,
    }
}

/// Saves the unified plan to the standard .autoresolve/plan.json path.
pub fn save_plan(path: &Path, plan: &Plan) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(plan)?)?;
    Ok(())
}
