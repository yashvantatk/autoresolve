use crate::agent::{run_agent, Tools};
use crate::detectors;
use crate::llm::{Provider, ToolSpec};
use crate::review::{submit_verdict_spec, Issue, Verdict};
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
or touch unrelated code. Change only what the bug requires: never rename or swap method calls \
on other objects, never change return types or signatures, and never bend production code to \
make a regression test pass. If the regression test seems to use the wrong kind of object, keep \
production code unchanged and say so in `summary`.";

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
    /// verified AND backed by a regression test that failed before and passes after
    pub proven: bool,
    /// (relative path, file content) of the regression test, if one exists
    pub test: Option<(String, String)>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReproTest {
    pub description: String,
    pub code: String,
}

const TESTER_SYSTEM: &str = "You write regression tests. Given a confirmed bug, write a minimal \
standalone Python script that reproduces it. The script must exit with an error (a failed assert \
or an uncaught exception) on the CURRENT buggy code, and exit cleanly once the bug is fixed. \
Rules: no pytest or other third-party packages, plain asserts only; import the code under test \
normally (for example `from buggy import last_item`), the repo root is already on sys.path; \
call the buggy code directly and assert on the expected correct behavior; never wrap the buggy \
call in a try/except that would let the script pass either way; test only the behavior in the \
claim; be deterministic, with no network or randomness. Read the code first with the tools, \
then call submit_test. Keep the script clean: no commentary about the task or your reasoning, \
at most one short comment. If the code under test needs a collaborator object (a cart, a client, \
a connection), import and use the REAL class from the repo; a stub is allowed only if it implements \
every method the code calls, under the same names. Never use a list or dict as a stand-in for an \
object type. The test must fail ONLY because of the claimed bug, and pass for any correct fix.";

fn submit_test_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_test",
        description: "Submit the regression test script. Call exactly once when done.",
        parameters: json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "One sentence: what the test checks."},
                "code": {"type": "string", "description": "Complete Python source of the script."}
            },
            "required": ["description", "code"]
        }),
    }
}

/// Safe filename fragment from a model-written title.
pub fn slugify(title: &str, n: usize) -> String {
    let s: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let s = s.split('_').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("_");
    let s: String = s.chars().take(40).collect();
    format!("{n}_{s}")
}

pub fn save_test(root: &Path, rel: &str, content: &str) -> Result<()> {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

/// Write the test (with a sys.path header so it can import repo modules) and return (path, content).
fn write_test(dir: &Path, slug: &str, code: &str) -> Result<(String, String)> {
    let rel = format!("autoresolve_regression/test_{slug}.py");
    let content = format!(
        "import sys, pathlib\nsys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))\n\n{code}\n"
    );
    save_test(dir, &rel, &content)?;
    Ok((rel, content))
}

/// Reproduction-first: get a test that FAILS on the current code. One retry with feedback.
pub async fn reproduce(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    issue: &Issue,
    slug: &str,
    max_steps: usize,
) -> Result<ReproTest> {
    let root = tools.root();
    let mut feedback = String::new();
    let mut passed_last = false;
    for attempt in 1..=2 {
        let mut specs = Tools::specs();
        specs.push(submit_test_spec());
        let mut task = format!(
            "Write a regression test for this confirmed bug:\n{}:{} [{}] {}\n{}",
            issue.file, issue.line, issue.severity, issue.title, issue.explanation
        );
        if !feedback.is_empty() {
            task.push_str(&format!("\n\nYour previous attempt was rejected:\n{feedback}"));
        }
        let out = run_agent(provider, tools, TESTER_SYSTEM, &task, specs, "submit_test", max_steps).await?;
        let t: ReproTest = serde_json::from_value(out).context("model returned a malformed test")?;

        let sandbox = create_sandbox(root, &format!("{slug}-repro{attempt}"))?;
        let (rel, _) = write_test(&sandbox, slug, &t.code)?;
        let (ok, tail) = run_tests(&sandbox, &format!("python3 {rel}"));
        let broken = tail.contains("SyntaxError")
            || tail.contains("ModuleNotFoundError")
            || !fails_for_real(&tail);
        passed_last = ok;
        if !ok && !broken {
            return Ok(t); // fails on the buggy code for a real reason, as required
        }
        feedback = if ok {
            "your test PASSED on the current buggy code; it must fail there".to_string()
        } else {
            format!("your test is broken, it does not fail because of the bug:\n{tail}")
        };
        eprintln!("[repro] attempt {attempt} rejected: {feedback}");
    }
    if passed_last {
        bail!("the test passes on the current code, so the bug looks already fixed")
    }
    bail!("could not write a test that fails on the current code")
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
            let tail: String = text.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
            (o.status.success(), tail)
        }
        Err(e) => (false, format!("could not run: {e}")),
    }
}

/// A failing test only counts as a reproduction if it fails by assertion, or the error
/// originates inside repo code. An error raised directly in the test file (a missing
/// attribute, a bad import) means the test is broken, not that it found the bug.
fn fails_for_real(tail: &str) -> bool {
    if tail.contains("AssertionError") {
        return true;
    }
    tail.lines()
        .filter(|l| l.trim_start().starts_with("File \""))
        .any(|l| !l.contains("autoresolve_regression"))
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

const PATCH_REVIEWER_SYSTEM: &str = "You are a strict reviewer judging a proposed PATCH, not the \
original bug. You get the bug claim and the unified diff of production code. Verdict `confirmed` \
means the patch is acceptable: every changed line is needed to fix the claimed bug and nothing \
else that other code relies on is altered. Verdict `refuted` means reject it: it changes anything \
the claim does not require, such as renaming or swapping method calls on other objects, changing \
return types, signatures or defaults of unrelated code, or editing code to suit a test stand-in. \
Use the tools to check how the changed code is really used (get_callers, and the real classes it \
talks to). Production code must never be bent to fit a test. If the task lists other confirmed bugs, a \
change that fixes one of them is acceptable only when it is minimal and needed to exercise the \
claimed bug. Finish by calling submit_verdict.";

pub async fn review_patch(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    issue: &Issue,
    diff: &str,
    others: &str,
    max_steps: usize,
) -> Result<Verdict> {
    let mut specs = Tools::specs();
    specs.push(submit_verdict_spec());
    let mut task = format!(
        "Bug claim:\n{}:{} {}\n{}\n\nProposed patch (unified diff):\n{}",
        issue.file, issue.line, issue.title, issue.explanation, diff
    );
    if !others.is_empty() {
        task.push_str(&format!(
            "\n\nOther confirmed bugs in this repo:\n{others}\nA change that fixes one of these is acceptable only if it is minimal and needed to exercise the claimed bug."
        ));
    }
    let out = run_agent(provider, tools, PATCH_REVIEWER_SYSTEM, &task, specs, "submit_verdict", max_steps).await?;
    serde_json::from_value(out).context("model returned a malformed verdict")
}

/// The regression test errored on the patched code: ask the tester to repair the TEST
/// (not the patch), then confirm it still fails on the original code.
async fn repair_test(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    issue: &Issue,
    old: &ReproTest,
    failure: &str,
    slug: &str,
    max_steps: usize,
) -> Result<ReproTest> {
    let root = tools.root();
    let mut specs = Tools::specs();
    specs.push(submit_test_spec());
    let task = format!(
        "Bug claim:\n{}:{} {}\n{}\n\nYour regression test:\n{}\n\nAfter a plausible fix was applied, the test still failed with:\n{}\n\n\
         The test is probably at fault (for example a stand-in object that lacks a method the code calls). \
         Rewrite the TEST so it uses the real classes from the repo, or a stub that has every method the code calls. \
         It must still fail on the original buggy code because of the claimed bug, and pass once the bug is fixed.",
        issue.file, issue.line, issue.title, issue.explanation, old.code, failure
    );
    let out = run_agent(provider, tools, TESTER_SYSTEM, &task, specs, "submit_test", max_steps).await?;
    let t: ReproTest = serde_json::from_value(out).context("model returned a malformed test")?;
    let sandbox = create_sandbox(root, &format!("{slug}-repair"))?;
    let (rel, _) = write_test(&sandbox, slug, &t.code)?;
    let (ok, tail) = run_tests(&sandbox, &format!("python3 {rel}"));
    if ok || tail.contains("SyntaxError") || tail.contains("ModuleNotFoundError") || !fails_for_real(&tail) {
        bail!("the repaired test no longer fails on the original code for the claimed bug");
    }
    Ok(t)
}

/// Ask the fixer for a patch, apply it in a sandbox, verify it. One retry with feedback.
pub async fn fix_issue(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    issue: &Issue,
    siblings: &[Issue],
    test_cmd: Option<&str>,
    repro: Option<(&str, &ReproTest)>,
    id: &str,
    max_steps: usize,
) -> Result<Outcome> {
    let root = tools.root();
    let slug: Option<&str> = repro.map(|(s, _)| s);
    let mut current: Option<ReproTest> = repro.map(|(_, t)| t.clone());
    let others: String = siblings
        .iter()
        .map(|s| format!("- {}:{} {}", s.file, s.line, s.title))
        .collect::<Vec<_>>()
        .join("\n");
    let mut feedback = String::new();
    let mut test_failure: Option<String> = None;
    let mut last: Option<Outcome> = None;

    for attempt in 1..=2 {
        // if the regression test itself errored on the patched code, let the tester repair it
        if let (Some(sl), Some(fail)) = (slug, test_failure.take()) {
            if let Some(old) = current.clone() {
                match repair_test(provider, tools, issue, &old, &fail, sl, max_steps).await {
                    Ok(t) => {
                        eprintln!("[fix] regression test rewritten after it errored on the patched code");
                        current = Some(t);
                    }
                    Err(e) => eprintln!("[fix] could not repair the test: {e}"),
                }
            }
        }

        let mut specs = Tools::specs();
        specs.push(submit_patch_spec());
        let mut task = format!(
            "Fix this confirmed bug:\n{}:{} [{}] {}\n{}\nSuggested fix: {}",
            issue.file, issue.line, issue.severity, issue.title, issue.explanation, issue.fix
        );
        if let Some(t) = &current {
            task.push_str(&format!(
                "\n\nA regression test for this bug exists and will be added to the repo automatically; it must pass after your fix. Edit ONLY existing source files, never create or edit test files:\n{}",
                t.code
            ));
        }
        if !others.is_empty() {
            task.push_str(&format!(
                "\n\nOther confirmed bugs exist in this repo:\n{others}\nIf your regression test cannot even run because one of them crashes first, you may include the minimal fix for that one too; say so in `summary`. Otherwise leave them alone."
            ));
        }
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

        // the regression test is added after patching, so it is not treated as a patched file
        let test_file = match (&current, slug) {
            (Some(t), Some(sl)) => Some(write_test(&sandbox, sl, &t.code)?),
            _ => None,
        };

        let mut checks = verify(&sandbox, root, &touched);
        if let Some((rel, _)) = &test_file {
            let (ok, tail) = run_tests(&sandbox, &format!("python3 {rel}"));
            // an error raised only in the test file means the TEST is wrong, not the patch
            if !ok && !fails_for_real(&tail) {
                test_failure = Some(tail.clone());
            }
            checks.push(Check {
                name: "regression test (failed before the patch)".into(),
                passed: ok,
                detail: if ok { "now passes".into() } else { tail },
            });
        }
        if let Some(cmd) = test_cmd {
            let (ok, tail) = run_tests(&sandbox, cmd);
            let base = baseline.map_or("n/a", |b| if b.0 { "pass" } else { "fail" });
            checks.push(Check {
                name: format!("tests `{cmd}`"),
                passed: ok,
                detail: if ok { format!("(baseline: {base})") } else { format!("(baseline: {base}) {tail}") },
            });
        }

        // Gate: even if every check passed, is the patch limited to what the bug requires?
        if checks.iter().all(|c| c.passed) {
            let prod_diff = diff_for(root, &sandbox, &touched);
            let gate = match review_patch(provider, tools, issue, &prod_diff, &others, max_steps).await {
                Ok(v) => Check {
                    name: "patch review (no unrelated changes)".into(),
                    passed: v.verdict == "confirmed",
                    detail: v.reason,
                },
                Err(e) => Check {
                    name: "patch review (no unrelated changes)".into(),
                    passed: false,
                    detail: format!("reviewer failed: {e}"),
                },
            };
            checks.push(gate);
        }

        let verified = !checks.is_empty() && checks.iter().all(|c| c.passed);
        let proven = verified && test_file.is_some();
        let mut shown = touched.clone();
        if let Some((rel, _)) = &test_file {
            shown.push(rel.clone());
        }
        let diff = diff_for(root, &sandbox, &shown);
        if verified {
            return Ok(Outcome { patch, checks, diff, verified, proven, test: test_file });
        }
        feedback = checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| format!("{}: {}", c.name, c.detail))
            .collect::<Vec<_>>()
            .join("\n");
        eprintln!("[fix] attempt {attempt} failed verification, retrying with feedback");
        last = Some(Outcome { patch, checks, diff, verified, proven, test: test_file });
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

    #[test]
    fn slugify_makes_safe_filenames() {
        assert_eq!(slugify("IndexError in last_item()!", 3), "3_indexerror_in_last_item");
        assert_eq!(slugify("../../etc/passwd", 0), "0_etc_passwd");
    }

    #[test]
    fn failure_must_come_from_the_bug_not_the_test() {
        let wrong = "Traceback (most recent call last):\n  File \"/x/autoresolve_regression/test_0.py\", line 7, in <module>\n    cart.check_item(1)\nAttributeError: 'Cart' object has no attribute 'check_item'\n";
        assert!(!fails_for_real(wrong));
        let right = "Traceback (most recent call last):\n  File \"/x/autoresolve_regression/test_0.py\", line 9, in <module>\n    add_all(c, [1])\n  File \"/x/buggy.py\", line 15, in add_all\n    check_item(i, strict=True)\nTypeError: check_item() got an unexpected keyword argument 'strict'\n";
        assert!(fails_for_real(right));
        assert!(fails_for_real("Traceback (most recent call last):\nAssertionError: expected 3\n"));
    }
}