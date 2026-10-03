//! Lint, security and type checks for patched files, run INSIDE the sandbox.
//! The rule: a patch may not add findings. For each touched file we count findings from
//! ruff (errors only: syntax errors and pyflakes, so style rules and ruff version upgrades never
//! fail a patch), bandit (security, medium severity and above) and mypy (types) before and after,
//! and require after <= before. Low-severity bandit notes are ignored on purpose: a correct security
//! fix often swaps one risky call for a safer one that adds a harmless low-severity note
//! (os.popen -> subprocess.run adds B404/B603), and that must not count against the patch.
//!
//! The default `python:3.12-slim` image has none of these tools, so the check turns itself off
//! (with one note) unless the sandbox image has them. Build one:
//!   docker build -t autoresolve-sandbox -f docker/sandbox.Dockerfile docker/
//!   export AUTORESOLVE_DOCKER_IMAGE=autoresolve-sandbox
//! `AUTORESOLVE_LINT=off` disables the check explicitly.

use crate::fix::Check;
use crate::sandbox;
use std::path::Path;
use std::sync::{Once, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub ruff: usize,
    pub bandit: usize,
    pub mypy: usize,
}

/// Single-quote a string for `sh -c`, so a file name can never inject commands.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// One shell command that prints `ruff=N`, `bandit=N` and `mypy=N` for a file.
/// Each tool's own exit code is ignored (they exit 1 when they find something).
pub fn count_command(file: &str) -> String {
    let f = shell_quote(file);
    format!(
        "echo \"ruff=$(ruff check --quiet --no-cache --select E9,F --output-format concise {f} 2>/dev/null | grep -c ': [A-Z]*[0-9]')\"; \
         echo \"bandit=$(bandit -q -ll -f custom --msg-template '{{relpath}}:{{line}}: {{test_id}}' {f} 2>/dev/null | grep -c ': B[0-9]')\"; \
         echo \"mypy=$(mypy --ignore-missing-imports --follow-imports=skip --no-error-summary --no-color-output --cache-dir=/dev/null {f} 2>/dev/null | grep -c ': error:')\""
    )
}

pub fn parse_counts(out: &str) -> Option<Counts> {
    let get = |key: &str| -> Option<usize> {
        out.lines()
            .find_map(|l| l.trim().strip_prefix(key).and_then(|v| v.strip_prefix('=')))
            .and_then(|v| v.trim().parse().ok())
    };
    Some(Counts { ruff: get("ruff")?, bandit: get("bandit")?, mypy: get("mypy")? })
}

fn enabled() -> bool {
    std::env::var("AUTORESOLVE_LINT").map(|v| v != "off").unwrap_or(true)
}

/// Are all three tools present in the sandbox image? Asked once per run.
fn available(dir: &Path) -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    static NOTE: Once = Once::new();
    let ok = *OK.get_or_init(|| sandbox::run(dir, "ruff --version && bandit --version && mypy --version", true).0);
    if !ok {
        NOTE.call_once(|| {
            eprintln!(
                "[lint] ruff, bandit or mypy not found in the sandbox image: lint checks skipped. \
                 To enable: docker build -t autoresolve-sandbox -f docker/sandbox.Dockerfile docker/ \
                 && export AUTORESOLVE_DOCKER_IMAGE=autoresolve-sandbox"
            );
        });
    }
    ok
}

/// None = check not run (disabled, tools missing, or no baseline). Some = a real pass/fail.
pub fn check(sandbox_dir: &Path, original_dir: &Path, file: &str) -> Option<Check> {
    if !enabled() || !available(sandbox_dir) {
        return None;
    }
    let cmd = count_command(file);
    let before = parse_counts(&sandbox::run(original_dir, &cmd, true).1)?;
    let name = format!("lint, security and types {file}");
    match parse_counts(&sandbox::run(sandbox_dir, &cmd, true).1) {
        Some(after) => Some(Check {
            name,
            passed: after.ruff <= before.ruff && after.bandit <= before.bandit && after.mypy <= before.mypy,
            detail: format!(
                "ruff {}->{}, bandit {}->{}, mypy {}->{}",
                before.ruff, after.ruff, before.bandit, after.bandit, before.mypy, after.mypy
            ),
        }),
        None => Some(Check { name, passed: false, detail: "could not read the linters' output".into() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_parsed_and_incomplete_output_is_rejected() {
        assert_eq!(
            parse_counts("ruff=2\nbandit=0\nmypy=11\n"),
            Some(Counts { ruff: 2, bandit: 0, mypy: 11 })
        );
        assert_eq!(parse_counts("ruff=2\nbandit=0\n"), None);
        assert_eq!(parse_counts("ruff=x\nbandit=0\nmypy=1\n"), None);
        assert_eq!(parse_counts("could not run: no docker"), None);
    }

    #[test]
    fn count_command_asks_bandit_for_medium_and_above_only() {
        assert!(count_command("a.py").contains("bandit -q -ll "));
    }

    /// Runs wherever the tools are installed: replacing os.popen with subprocess.run(shlex.split(..))
    /// is a security improvement and must pass even though it adds low-severity bandit notes.
    #[test]
    fn a_safer_call_that_adds_only_low_severity_notes_passes() {
        let base = std::env::temp_dir().join(format!("autoresolve-lint-sec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (before, after) = (base.join("before"), base.join("after"));
        std::fs::create_dir_all(&before).unwrap();
        std::fs::create_dir_all(&after).unwrap();
        std::fs::write(before.join("m.py"), "import os\n\ndef run(cmd):\n    return os.popen(cmd).read()\n").unwrap();
        std::fs::write(
            after.join("m.py"),
            "import shlex\nimport subprocess\n\ndef run(cmd):\n    return subprocess.run(shlex.split(cmd), capture_output=True, text=True).stdout\n",
        )
        .unwrap();
        if let Some(c) = check(&after, &before, "m.py") {
            assert!(c.passed, "a safer call must not fail on low-severity notes: {}", c.detail);
        }
    }

    #[test]
    fn file_names_cannot_inject_shell_commands() {
        let evil = "a b'$(touch pwned).py";
        let q = shell_quote(evil);
        // the shell must hand the original string back unchanged
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {q}"))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), evil);
        assert!(count_command(evil).contains(&q));
    }

    /// Runs for real wherever ruff, bandit and mypy are installed; elsewhere `check` returns None and this passes.
    #[test]
    fn a_patch_that_adds_findings_fails_and_one_that_removes_them_passes() {
        let base = std::env::temp_dir().join(format!("autoresolve-lint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (before, worse, better) = (base.join("before"), base.join("worse"), base.join("better"));
        for d in [&before, &worse, &better] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(before.join("m.py"), "import os\n\ndef f():\n    return 1\n").unwrap();
        std::fs::write(worse.join("m.py"), "import os\nimport sys\n\ndef f():\n    return 1\n").unwrap();
        std::fs::write(better.join("m.py"), "def f():\n    return 1\n").unwrap();
        if let Some(c) = check(&worse, &before, "m.py") {
            assert!(!c.passed, "adding an unused import must fail: {}", c.detail);
        }
        if let Some(c) = check(&better, &before, "m.py") {
            assert!(c.passed, "removing an unused import must pass: {}", c.detail);
        }
    }
}