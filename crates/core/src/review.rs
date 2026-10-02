use crate::agent::{run_agent, Tools};
use crate::llm::{Provider, ToolCall, ToolSpec};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub severity: String,
    pub file: String,
    pub line: u32,
    pub title: String,
    pub explanation: String,
    pub fix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub verdict: String, // "confirmed" | "refuted" | "uncertain"
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Judged {
    pub issue: Issue,
    pub verdict: Verdict,
}

const REVIEWER_SYSTEM: &str = "You are a senior code reviewer working inside a repository. \
Investigate with the tools: read the code, check callers and callees, and run static_findings \
(its results are hints, not proof). Report only real bugs and security problems that you \
verified by reading the code. Skip style nitpicks. When finished, call submit_findings \
exactly once, with an empty list if nothing is wrong.";

const SKEPTIC_SYSTEM: &str = "You are a skeptical senior engineer. A colleague claims to have \
found a bug. Your job is to try to REFUTE the claim. Read the exact code, check callers and \
callees for guards or validation, and check whether the failing input is actually reachable \
and the described behavior is real. Mark `confirmed` only if you can point to the specific \
lines that make the bug real. Mark `refuted` if the claim is wrong or the case cannot happen. \
Mark `uncertain` if you cannot tell. Finish by calling submit_verdict.";

fn submit_findings_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_findings",
        description: "Submit the final list of verified issues. Call exactly once when done.",
        parameters: json!({
            "type": "object",
            "properties": {"findings": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "severity": {"type": "string", "enum": ["high", "medium", "low"]},
                    "file": {"type": "string"},
                    "line": {"type": "integer"},
                    "title": {"type": "string"},
                    "explanation": {"type": "string"},
                    "fix": {"type": "string"}
                },
                "required": ["severity", "file", "line", "title", "explanation", "fix"]
            }}},
            "required": ["findings"]
        }),
    }
}

pub fn submit_verdict_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_verdict",
        description: "Submit your verdict on the claimed bug. Call exactly once when done.",
        parameters: json!({
            "type": "object",
            "properties": {
                "verdict": {"type": "string", "enum": ["confirmed", "refuted", "uncertain"]},
                "reason": {"type": "string", "description": "Cite the specific lines or callers."}
            },
            "required": ["verdict", "reason"]
        }),
    }
}

pub async fn find_issues(provider: &dyn Provider, tools: &Tools<'_>, target: &str, max_steps: usize) -> Result<Vec<Issue>> {
    let mut specs = Tools::specs();
    specs.push(submit_findings_spec());

    // Deterministic AST detectors run first; their findings become candidates for the model to verify.
    let hints = tools.call(&ToolCall { name: "static_findings".into(), args: json!({"file": target}) });
    let hint_text = hints
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| {
                    format!(
                        "- line {}: [{}] {}",
                        f["line"],
                        f["rule"].as_str().unwrap_or(""),
                        f["message"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let mut task = format!(
        "Review `{target}` for real bugs and security problems. Start by reading the file with \
         read_lines (up to 200 lines per call), then call submit_findings."
    );
    if !hint_text.is_empty() {
        eprintln!("[reviewer] static scanner found candidates:\n{hint_text}");
        task.push_str(&format!(
            "\n\nA deterministic scanner already flagged these candidates:\n{hint_text}\n\
             Verify each against the code and include the real ones. Then look for problems the scanner \
             cannot see, such as wrong arguments, wrong indexes, unhandled edge cases and logic errors."
        ));
    }

    let out = run_agent(provider, tools, REVIEWER_SYSTEM, &task, specs, "submit_findings", max_steps).await?;
    if out["findings"].is_null() {
        eprintln!("[reviewer] submitted no `findings` field (treating as no issues): {out}");
        return Ok(vec![]); // models sometimes omit an empty list
    }
    serde_json::from_value(out["findings"].clone()).context("model returned malformed findings")
}

pub async fn challenge(provider: &dyn Provider, tools: &Tools<'_>, issue: &Issue, max_steps: usize) -> Result<Verdict> {
    let mut specs = Tools::specs();
    specs.push(submit_verdict_spec());
    let task = format!(
        "Claim to challenge:\n{}:{} [{}] {}\n{}\nProposed fix: {}",
        issue.file, issue.line, issue.severity, issue.title, issue.explanation, issue.fix
    );
    let out = run_agent(provider, tools, SKEPTIC_SYSTEM, &task, specs, "submit_verdict", max_steps).await?;
    serde_json::from_value(out).context("model returned a malformed verdict")
}

/// Reviewer proposes, Skeptic disposes.
pub async fn review(provider: &dyn Provider, tools: &Tools<'_>, target: &str, max_steps: usize) -> Result<Vec<Judged>> {
    let issues = find_issues(provider, tools, target, max_steps).await?;
    eprintln!("[reviewer] proposed {} issue(s); handing to the skeptic", issues.len());
    let mut out = Vec::new();
    for issue in issues {
        eprintln!("[skeptic] challenging: {}", issue.title);
        let verdict = match challenge(provider, tools, &issue, max_steps).await {
            Ok(v) => v,
            Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
            // one failed challenge should not sink the whole review
            Err(e) => Verdict { verdict: "uncertain".into(), reason: format!("skeptic failed: {e}") },
        };
        out.push(Judged { issue, verdict });
    }
    Ok(out)
}