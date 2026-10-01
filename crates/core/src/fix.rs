use crate::agent::{run_agent, Tools};
use crate::detectors;
use crate::llm::{Provider, ToolSpec};
use crate::review::Issue;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

const FIXER_SYSTEM: &str = "You are a careful engineer fixing ONE confirmed bug. Read the \
relevant code with the tools, then make the smallest correct change. Submit it with \
submit_patch as search/replace edits. Each `search` must match the file text EXACTLY, \
including indentation, and must occur exactly once in that file, so include enough \
surrounding lines to be unique. The read_lines tool prefixes each line with a line number \
and a '|' character; never include that prefix in `search` or `replace`. Do not reformat \
or touch unrelated code.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edit {
    pub file: String,
    pub search: String,
    pub replace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub summary: String,
    pub edits: Vec<Edit>,
}

#[derive(Debug)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug)]
pub struct Outcome {
    pub patch: Patch,
    pub checks: Vec<Check>,
    pub diff: String,
    pub verified: bool,
}

fn submit_patch_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_patch",
        description: "Submit the fix as search/replace edits. Call exactly once when done.",
        parameters: json!({
            "type": "object",
            "properties": {
                "summary": {"type": "string", "description": "One sentence: what the patch changes."},
                "edits": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string"},
                        "search": {"type": "string"},
                        "replace": {"type": "string"}
                    },
                    "required": ["file", "search", "replace"]
                }}
            },
            "required": ["summary", "edits"]
        }),
    }
}

fn safe_join(base: &Path, file: &str) -> Result<PathBuf> {
    let p = base
        .join(file)
        .canonicalize()
        .with_context(|| format!("no such file: {file}"))?;
    if !p.starts_with(base) {
        bail!("path escapes the base directory");
    }
    Ok(p)
}

/// Apply edits under `base`. All-or-nothing: nothing is written unless every edit matches.
pub fn apply_edits(base: &Path, edits: &[Edit]) -> Result<Vec<String>> {
    let base = base.canonicalize().context("bad base directory")?;
    let mut pending: HashMap<PathBuf, String> = HashMap::new();
    let mut touched: Vec<String> = Vec::new();
    for e in edits {
        let path = safe_join(&base, &e.file)?;
        let current = match pending.get(&path) {
            Some(t) => t.clone(),
            None => std::fs::read_to_string(&path)?,
        };
        let n = if e.search.is_empty() { 0 } else { current.matches(&e.search).count() };
        if n != 1 {
            bail!("edit for {}: `search` must match exactly once but matched {n} time(s)", e.file);
        }
        pending.insert(path, current.replacen(&e.search, &e.replace, 1));
        if !touched.contains(&e.file) {
            touched.push(e.file.clone());
        }
    }
    for (path, text) in pending {
        std::fs::write(path, text)?;
    }
    Ok(touched)
}

/// Copy the repo (respecting .gitignore) into .autoresolve/sandbox/<id>.
pub fn create_sandbox(root: &Path, id: &str) -> Result<PathBuf> {
    let dir = root.join(".autoresolve").join("sandbox").join(id);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let walker = ignore::WalkBuilder::new(root)
        .filter_entry(|e| {
            let n = e.file_name();
            n != "target" && n != "__pycache__"
        })
        .build();
    for entry in walker {
        let entry = entry?;
        let rel = entry.path().strip_prefix(root)?;
        let dest = dir.join(rel);
        match entry.file_type() {
            Some(t) if t.is_dir() => std::fs::create_dir_all(&dest)?,
            Some(t) if t.is_file() => {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(entry.path(), &dest)?;
            }
            _ => {}
        }
    }
    Ok(dir.canonicalize()?)
}

/// Static checks on every touched Python file.
pub fn verify(sandbox: &Path, original_root: &Path, touched: &[String]) -> Vec<Check> {
    let mut checks = Vec::new();
    for f in touched.iter().filter(|f| f.ends_with(".py")) {
        let src = std::fs::read_to_string(sandbox.join(f)).unwrap_or_default();
        let syntax_ok = crate::parse_python(&src).map(|t| !t.root_node().has_error()).unwrap_or(false);
        checks.push(Check { name: format!("syntax {f}"), passed: syntax_ok, detail: String::new() });

        let before = std::fs::read_to_string(original_root.join(f))
            .ok()
            .and_then(|s| detectors::scan_python(Path::new(f), &s).ok())
            .map_or(0, |v| v.len());
        let after = detectors::scan_python(Path::new(f), &src).map_or(usize::MAX, |v| v.len());
        checks.push(Check {
            name: format!("no new anti-patterns {f}"),
            passed: after <= before,
            detail: format!("{before} -> {after} findings"),
        });
    }
    checks
}

fn run_tests(dir: &Path, cmd: &str) -> (bool, String) {
    match Command::new("timeout").args(["120", "sh", "-c", cmd]).current_dir(dir).output() {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            let tail: String = text.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
            (o.status.success(), tail)
        }
        Err(e) => (false, format!("could not run: {e}")),
    }
}

fn diff_for(root: &Path, sandbox: &Path, files: &[String]) -> String {
    let mut out = String::new();
    for f in files {
        let old = std::fs::read_to_string(root.join(f)).unwrap_or_default();
        let new = std::fs::read_to_string(sandbox.join(f)).unwrap_or_default();
        let diff = similar::TextDiff::from_lines(old.as_str(), new.as_str());
        out.push_str(
            &diff
                .unified_diff()
                .context_radius(2)
                .header(&format!("a/{f}"), &format!("b/{f}"))
                .to_string(),
        );
    }
    out
}

/// Ask the fixer for a patch, apply it in a sandbox, verify it. One retry with feedback.
pub async fn fix_issue(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    issue: &Issue,
    test_cmd: Option<&str>,
    id: &str,
    max_steps: usize,
) -> Result<Outcome> {
    let root = tools.root();
    let mut feedback = String::new();
    let mut last: Option<Outcome> = None;

    for attempt in 1..=2 {
        let mut specs = Tools::specs();
        specs.push(submit_patch_spec());
        let mut task = format!(
            "Fix this confirmed bug:\n{}:{} [{}] {}\n{}\nSuggested fix: {}",
            issue.file, issue.line, issue.severity, issue.title, issue.explanation, issue.fix
        );
        if !feedback.is_empty() {
            task.push_str(&format!("\n\nYour previous attempt failed:\n{feedback}\nFix that and try again."));
        }
        let out = run_agent(provider, tools, FIXER_SYSTEM, &task, specs, "submit_patch", max_steps).await?;
        let patch: Patch = serde_json::from_value(out).context("model returned a malformed patch")?;

        let sandbox = create_sandbox(root, &format!("{id}-{attempt}"))?;
        let baseline = test_cmd.map(|c| run_tests(&sandbox, c));
        let touched = match apply_edits(&sandbox, &patch.edits) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[fix] attempt {attempt}: patch did not apply: {e}");
                feedback = e.to_string();
                continue;
            }
        };

        let mut checks = verify(&sandbox, root, &touched);
        if let Some(cmd) = test_cmd {
            let (ok, tail) = run_tests(&sandbox, cmd);
            let base = baseline.map_or("n/a", |b| if b.0 { "pass" } else { "fail" });
            checks.push(Check {
                name: format!("tests `{cmd}`"),
                passed: ok,
                detail: if ok { format!("(baseline: {base})") } else { format!("(baseline: {base}) {tail}") },
            });
        }
        let verified = !checks.is_empty() && checks.iter().all(|c| c.passed);
        let diff = diff_for(root, &sandbox, &touched);
        if verified {
            return Ok(Outcome { patch, checks, diff, verified });
        }
        feedback = checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| format!("{}: {}", c.name, c.detail))
            .collect::<Vec<_>>()
            .join("\n");
        eprintln!("[fix] attempt {attempt} failed verification, retrying with feedback");
        last = Some(Outcome { patch, checks, diff, verified });
    }
    last.with_context(|| format!("no patch could be applied: {feedback}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("autoresolve-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn edit(f: &str, s: &str, r: &str) -> Edit {
        Edit { file: f.into(), search: s.into(), replace: r.into() }
    }

    #[test]
    fn edits_must_match_exactly_once() {
        let d = tmp("edits");
        std::fs::write(d.join("f.py"), "x = 1\ny = 1\n").unwrap();
        assert!(apply_edits(&d, &[edit("f.py", "x = 1", "x = 2")]).is_ok());
        assert_eq!(std::fs::read_to_string(d.join("f.py")).unwrap(), "x = 2\ny = 1\n");
        assert!(apply_edits(&d, &[edit("f.py", "= ", "= 9")]).is_err()); // matches twice
        assert!(apply_edits(&d, &[edit("f.py", "zzz", "q")]).is_err()); // matches never
        assert!(apply_edits(&d, &[edit("../escape.py", "a", "b")]).is_err()); // outside base
    }

    #[test]
    fn verify_catches_syntax_errors() {
        let d = tmp("verify");
        std::fs::write(d.join("bad.py"), "def f(:\n    pass\n").unwrap();
        std::fs::write(d.join("good.py"), "def f():\n    pass\n").unwrap();
        let bad = verify(&d, &d, &["bad.py".to_string()]);
        assert!(bad.iter().any(|c| c.name.starts_with("syntax") && !c.passed));
        let good = verify(&d, &d, &["good.py".to_string()]);
        assert!(good.iter().all(|c| c.passed));
    }

    #[test]
    fn sandbox_copy_is_independent() {
        let d = tmp("sandbox");
        std::fs::write(d.join("a.py"), "x = 1\n").unwrap();
        let root = d.canonicalize().unwrap();
        let sb = create_sandbox(&root, "t").unwrap();
        apply_edits(&sb, &[edit("a.py", "x = 1", "x = 2")]).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.py")).unwrap(), "x = 1\n");
        assert_eq!(std::fs::read_to_string(sb.join("a.py")).unwrap(), "x = 2\n");
    }
}