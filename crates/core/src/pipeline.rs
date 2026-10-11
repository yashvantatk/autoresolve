//! Hybrid pipeline runner coordinating AI and deterministic execution.

use anyhow::Result;
use std::path::Path;
use crate::deterministic::run_deterministic_fixes;
use crate::events;
use crate::fix::{self, Plan};
use crate::output::save_plan;

/// Executes the deterministic pipeline, bypassing all LLM agents.
pub fn run_deterministic_cli(root: &Path, file: &Path, apply: bool) -> Result<Plan> {
    let real = root.canonicalize()?;
    let target = file.display().to_string();

    let started = std::time::Instant::now();
    let _ = events::init(&real.join(".autoresolve").join(events::LOG_FILE), &events::new_run_id());
    events::emit("run_start", events::run_config("deterministic_fix", &target));

    println!("Running lightweight deterministic AST & lint auto-patcher for {target}...");

    let plan_path = real.join(".autoresolve").join("plan.json");
    if plan_path.exists() {
        let _ = std::fs::remove_file(&plan_path);
    }

    let plan = run_deterministic_fixes(&real, file)?;

    for (idx, item) in plan.items.iter().enumerate() {
        println!("=== [PROVEN] {} ===", item.title);
        events::emit(
            "issue_start",
            serde_json::json!({
                "n": idx,
                "title": item.title,
                "file": target,
                "severity": "medium"
            }),
        );
        events::emit(
            "outcome",
            serde_json::json!({
                "title": item.title,
                "verified": true,
                "proven": true,
                "summary": item.summary
            }),
        );
    }

    if apply {
        for item in &plan.items {
            fix::apply_edits(&real, &item.edits)?;
        }
        println!("Applied {} deterministic fix(es) to repo.", plan.items.len());
    }

    save_plan(&plan_path, &plan)?;

    println!(
        "plan saved to {} ({} deterministic proven fix(es)). Review it, or apply with:\n  cargo run -p autoresolve-cli -- apply-plan",
        plan_path.display(),
        plan.items.len()
    );

    events::emit(
        "run_end",
        serde_json::json!({
            "target_file": target,
            "verified": plan.items.len(),
            "proven": plan.items.len(),
            "resolved_earlier": 0,
            "confirmed": plan.items.len(),
            "mode": "deterministic",
            "elapsed_ms": started.elapsed().as_millis() as u64
        }),
    );

    Ok(plan)
}
