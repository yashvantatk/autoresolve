use crate::agent::{run_agent, Tools};
use crate::detectors;
use crate::llm::{Provider, ToolSpec};
use crate::policy::{self, Policy};
use crate::review::{submit_verdict_spec, Issue, Verdict};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

/// One verified fix, as saved in the plan file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub title: String,
    pub summary: String,
    pub proven: bool,
    pub edits: Vec<Edit>,
    /// (relative path, content) of the regression test
    pub test: Option<(String, String)>,
}

/// Everything a `fix` run verified, in the order it was stacked.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Plan {
    pub items: Vec<PlanItem>,
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
    /// Text the failure output of the buggy code must contain (e.g. "IndexError").
    #[serde(default)]
    pub expected_failure: String,
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
object type. The test must fail ONLY because of the claimed bug, and pass for any correct fix. \
Also report `expected_failure`: a short text that the failure output of the buggy code will contain, \
taken from the bug claim. Usually this is an exception class name such as IndexError or TypeError, \
or AssertionError when the bug is a wrong result. Use an exact message only if you are certain of it. \
A test that fails in any other way is rejected.";

fn submit_test_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_test",
        description: "Submit the regression test script. Call exactly once when done.",
        parameters: json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "One sentence: what the test checks."},
                "code": {"type": "string", "description": "Complete Python source of the script."},
                "expected_failure": {"type": "string", "description": "Short text the failure output on the buggy code will contain, e.g. IndexError, TypeError or AssertionError."}
            },
            "required": ["description", "code", "expected_failure"]
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

/// Drop comment-only lines (models like to think out loud in them) and collapse blank runs.
fn clean_test_code(code: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in code.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.trim().is_empty() && out.last().map_or(true, |l| l.trim().is_empty()) {
            continue;
        }
        out.push(line.trim_end());
    }
    out.join("\n").trim().to_string()
}

/// The failure must match what the bug claim predicts. An empty expectation means no check.
fn matches_expected(tail: &str, expected: &str) -> bool {
    let e = expected.trim();
    e.is_empty() || tail.contains(e)
}

fn last_line(tail: &str) -> String {
    tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

pub fn save_test(root: &Path, rel: &str, content: &str) -> Result<()> {
    // no `..`, no absolute paths, and never through a symlink (it could point outside the repo)
    policy::reject_symlinks(root, rel)?;
    let path = root.join(policy::normalize(rel)?);
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

/// Reproduction-first: get a test that FAILS on the current code, the way the claim predicts.
/// One retry with feedback.
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
        let mut t: ReproTest = serde_json::from_value(out).context("model returned a malformed test")?;
        t.code = clean_test_code(&t.code);

        let sandbox = create_sandbox(root, &format!("{slug}-repro{attempt}"))?;
        let (rel, _) = write_test(&sandbox, slug, &t.code)?;
        let (ok, tail) = run_tests(&sandbox, &format!("python3 {rel}"));
        let broken = tail.contains("SyntaxError")
            || tail.contains("ModuleNotFoundError")
            || !fails_for_real(&tail);
        passed_last = ok;
        if !ok && !broken && matches_expected(&tail, &t.expected_failure) {
            return Ok(t); // fails on the buggy code, the way the claim predicts
        }
        feedback = if ok {
            "your test PASSED on the current buggy code; it must fail there".to_string()
        } else if !broken {
            format!(
                "your test fails, but not the way the bug claim predicts. You said it would fail with `{}`, but it failed with: {}",
                t.expected_failure.trim(),
                last_line(&tail)
            )
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

/// Where scratch copies live. Always under the REAL repo's `.autoresolve/sandbox`, even when
/// `root` is itself a scratch copy (the staging copy), so copies never nest inside each other.
fn scratch_base(root: &Path) -> PathBuf {
    for a in root.ancestors() {
        if a.file_name().is_some_and(|n| n == ".autoresolve") {
            return a.join("sandbox");
        }
    }
    root.join(".autoresolve").join("sandbox")
}

/// Delete every scratch copy of the repo at `root` (never plan.json or the graph database).
/// Refuses to follow a symlink, so a planted `.autoresolve/sandbox` link cannot aim the delete elsewhere.
pub fn clean_scratch(root: &Path) {
    let base = root.join(".autoresolve").join("sandbox");
    match std::fs::symlink_metadata(&base) {
        Ok(m) if m.file_type().is_dir() => {
            let _ = std::fs::remove_dir_all(&base);
        }
        Ok(m) if m.file_type().is_symlink() => {
            let _ = std::fs::remove_file(&base);
        }
        _ => {}
    }
}

/// Copy the repo (respecting .gitignore, skipping hidden files such as `.env` and `.git`)
/// into .autoresolve/sandbox/<id>.
pub fn create_sandbox(root: &Path, id: &str) -> Result<PathBuf> {
    let dir = scratch_base(root).join(id);
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

/// Apply a saved plan to the real repo. The whole plan is rehearsed on a scratch copy first,
/// so a stale plan (the repo changed since) fails without touching anything.
pub fn apply_plan(root: &Path, plan: &Plan, policy: &Policy) -> Result<()> {
    let root = root.canonicalize().context("bad repo root")?;
    // the plan file is just a file on disk: re-check every item against the policy before anything else
    policy.check_plan(plan)?;
    for p in &plan.items {
        for e in &p.edits {
            policy::reject_symlinks(&root, &e.file)?;
        }
    }
    let scratch = create_sandbox(&root, "apply-check")?;
    for (i, p) in plan.items.iter().enumerate() {
        apply_edits(&scratch, &p.edits).with_context(|| {
            format!(
                "plan item {} ({}) no longer applies; the repo changed since the plan was made. Nothing was written.",
                i + 1,
                p.title
            )
        })?;
    }
    let _ = std::fs::remove_dir_all(&scratch);
    for p in &plan.items {
        apply_edits(&root, &p.edits)?;
        if let Some((rel, content)) = &p.test {
            save_test(&root, rel, content)?;
        }
    }
    Ok(())
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

pub fn run_tests(dir: &Path, cmd: &str) -> (bool, String) {
    // sandbox copies are disposable, so the working directory is mounted writable
    crate::sandbox::run(dir, cmd, false)
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
/// (not the patch), then confirm it still fails on the original code the way the claim predicts.
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
    let mut t: ReproTest = serde_json::from_value(out).context("model returned a malformed test")?;
    t.code = clean_test_code(&t.code);
    let sandbox = create_sandbox(root, &format!("{slug}-repair"))?;
    let (rel, _) = write_test(&sandbox, slug, &t.code)?;
    let (ok, tail) = run_tests(&sandbox, &format!("python3 {rel}"));
    if ok
        || tail.contains("SyntaxError")
        || tail.contains("ModuleNotFoundError")
        || !fails_for_real(&tail)
        || !matches_expected(&tail, &t.expected_failure)
    {
        bail!("the repaired test no longer fails on the original code for the claimed bug");
    }
    Ok(t)
}

/// Ask the fixer for a patch, apply it in a sandbox, verify it. One retry with feedback.
/// `provider` writes the patch (a cheap model is fine: the checks catch its mistakes).
/// `gate` judges the finished patch (its mistakes are silent, so give it the strongest model).
#[allow(clippy::too_many_arguments)]
pub async fn fix_issue(
    provider: &dyn Provider,
    gate: &dyn Provider,
    tools: &Tools<'_>,
    issue: &Issue,
    siblings: &[Issue],
    test_cmd: Option<&str>,
    repro: Option<(&str, &ReproTest)>,
    policy: &Policy,
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
        // if the regression test itself looks wrong on the patched code, let the tester repair it
        if let (Some(sl), Some(fail)) = (slug, test_failure.take()) {
            if let Some(old) = current.clone() {
                match repair_test(provider, tools, issue, &old, &fail, sl, max_steps).await {
                    Ok(t) => {
                        eprintln!("[fix] regression test rewritten after it misbehaved on the patched code");
                        current = Some(t);
                    }
                    Err(e) => eprintln!("[fix] could not repair the test: {e}"),
                }
            }
        }
        let expected = current.as_ref().map(|t| t.expected_failure.clone()).unwrap_or_default();

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

        // policy first: a patch that touches protected paths or is too large never reaches the sandbox
        if let Err(e) = policy.check_edits(&patch.edits) {
            eprintln!("[fix] attempt {attempt}: {e}");
            feedback = e.to_string();
            continue;
        }

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
            // a failure that is not the one the claim predicts usually means the TEST is wrong
            if !ok && (!fails_for_real(&tail) || !matches_expected(&tail, &expected)) {
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
            let verdict = match review_patch(gate, tools, issue, &prod_diff, &others, max_steps).await {
                Ok(v) => Check {
                    name: "patch review (no unrelated changes)".into(),
                    passed: v.verdict == "confirmed",
                    detail: v.reason,
                },
                // out of daily quota is not a verdict: stop the run instead of rejecting every patch
                Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
                Err(e) => Check {
                    name: "patch review (no unrelated changes)".into(),
                    passed: false,
                    detail: format!("reviewer failed: {e}"),
                },
            };
            checks.push(verdict);
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

    #[test]
    fn failure_must_match_what_the_claim_predicts() {
        let tail = "Traceback (most recent call last):\nAttributeError: 'list' object has no attribute 'add'\n";
        assert!(!matches_expected(tail, "unexpected keyword argument 'strict'"));
        assert!(matches_expected(tail, "AttributeError"));
        assert!(matches_expected(tail, "  ")); // empty expectation means no check
        assert_eq!(last_line(tail), "AttributeError: 'list' object has no attribute 'add'");
    }

    #[test]
    fn test_code_loses_its_thinking_out_loud() {
        let raw = "from buggy import f\n\n# Wait, let me re-read the prompt.\n# Hmm.\n\n\nassert f() == 1  # inline comments stay\n";
        assert_eq!(clean_test_code(raw), "from buggy import f\n\nassert f() == 1  # inline comments stay");
    }

    #[test]
    fn plan_applies_in_order_or_not_at_all() {
        let d = tmp("plan");
        std::fs::write(d.join("a.py"), "x = 1\n").unwrap();
        let plan = Plan {
            items: vec![
                PlanItem {
                    title: "a".into(),
                    summary: String::new(),
                    proven: false,
                    edits: vec![edit("a.py", "x = 1", "x = 2")],
                    test: None,
                },
                PlanItem {
                    title: "b".into(),
                    summary: String::new(),
                    proven: true,
                    // stacked: this edit only matches after the first one is applied
                    edits: vec![edit("a.py", "x = 2", "x = 3")],
                    test: Some(("autoresolve_regression/test_b.py".into(), "pass\n".into())),
                },
            ],
        };
        apply_plan(&d, &plan, &Policy::default()).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("a.py")).unwrap(), "x = 3\n");
        assert!(d.join("autoresolve_regression/test_b.py").exists());

        // a stale plan (the file changed since) must not touch anything
        std::fs::write(d.join("a.py"), "y = 0\n").unwrap();
        assert!(apply_plan(&d, &plan, &Policy::default()).is_err());
        assert_eq!(std::fs::read_to_string(d.join("a.py")).unwrap(), "y = 0\n");
    }

    fn plan_of(edits: Vec<Edit>, test: Option<(String, String)>) -> Plan {
        Plan {
            items: vec![PlanItem { title: "t".into(), summary: String::new(), proven: false, edits, test }],
        }
    }

    #[test]
    fn apply_plan_refuses_a_tampered_plan_and_writes_nothing() {
        let d = tmp("tamper");
        std::fs::write(d.join("a.py"), "x = 1\n").unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git/config"), "keep\n").unwrap();
        let pol = Policy::default();

        // edit aimed at .git
        let git = plan_of(vec![edit(".git/config", "keep", "pwned")], None);
        assert!(apply_plan(&d, &git, &pol).is_err());
        assert_eq!(std::fs::read_to_string(d.join(".git/config")).unwrap(), "keep\n");

        // test path that escapes the repo
        let esc = plan_of(vec![edit("a.py", "x = 1", "x = 2")], Some(("../escaped.py".into(), "x\n".into())));
        assert!(apply_plan(&d, &esc, &pol).is_err());
        assert_eq!(std::fs::read_to_string(d.join("a.py")).unwrap(), "x = 1\n"); // nothing was written
        assert!(!d.parent().unwrap().join("escaped.py").exists());
    }

    #[test]
    fn symlinks_cannot_redirect_writes() {
        let d = tmp("links");
        let outside = tmp("links-outside");
        // a regression directory that is really a link to somewhere else
        std::os::unix::fs::symlink(&outside, d.join("autoresolve_regression")).unwrap();
        assert!(save_test(&d, "autoresolve_regression/test_0_x.py", "pass\n").is_err());
        assert!(!outside.join("test_0_x.py").exists());

        // an edit target that is a symlink
        std::fs::write(d.join("real.py"), "x = 1\n").unwrap();
        std::os::unix::fs::symlink(d.join("real.py"), d.join("link.py")).unwrap();
        let plan = plan_of(vec![edit("link.py", "x = 1", "x = 2")], None);
        assert!(apply_plan(&d, &plan, &Policy::default()).is_err());
        assert_eq!(std::fs::read_to_string(d.join("real.py")).unwrap(), "x = 1\n");

        // ordinary saves still work
        assert!(save_test(&d, "plain/test_0_y.py", "pass\n").is_ok());
    }

    #[test]
    fn scratch_copies_never_nest_and_cleanup_is_contained() {
        let d = tmp("nest");
        std::fs::write(d.join("a.py"), "x = 1\n").unwrap();
        std::fs::create_dir_all(d.join(".autoresolve")).unwrap();
        std::fs::write(d.join(".autoresolve/plan.json"), "{}").unwrap();
        let root = d.canonicalize().unwrap();
        let work = create_sandbox(&root, "work").unwrap();
        let inner = create_sandbox(&work, "inner").unwrap();
        // the copy made from the staging copy sits beside it, not inside it
        assert_eq!(inner, root.join(".autoresolve/sandbox/inner"));
        assert!(!work.join(".autoresolve").exists());
        assert!(inner.join("a.py").exists());
        // cleanup removes scratch copies but keeps plan.json
        clean_scratch(&root);
        assert!(!root.join(".autoresolve/sandbox").exists());
        assert!(root.join(".autoresolve/plan.json").exists());
        assert!(root.join("a.py").exists());
    }

    #[test]
    fn cleanup_does_not_follow_a_symlinked_sandbox_dir() {
        let d = tmp("cleanlink");
        let victim = tmp("cleanlink-victim");
        std::fs::write(victim.join("precious.txt"), "keep").unwrap();
        std::fs::create_dir_all(d.join(".autoresolve")).unwrap();
        std::os::unix::fs::symlink(&victim, d.join(".autoresolve/sandbox")).unwrap();
        clean_scratch(&d);
        assert!(victim.join("precious.txt").exists());
    }
}