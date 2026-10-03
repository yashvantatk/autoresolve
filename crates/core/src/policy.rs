//! Policy: what a patch is allowed to touch.
//! Checked when a patch is proposed AND again at apply-plan, so a plan file that was edited
//! by hand (or produced by a buggy run) cannot write anywhere the policy forbids.
//!
//! Optional `policy.toml` in the repo root:
//!   protected_paths     = ["tests/**", "setup.py"]   # extra paths no patch may edit
//!   max_files_per_patch = 3
//!   max_changed_lines   = 40
//! Matching is against the repo-relative path with `/` separators. `*` does not cross `/`;
//! use `**` to cross directories (e.g. `tests/**`, `**/conftest.py`). Case-insensitive.

use crate::fix::{Edit, Plan};
use anyhow::{bail, Context, Result};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::path::{Component, Path};

pub const POLICY_FILE: &str = "policy.toml";
pub const TEST_DIR: &str = "autoresolve_regression";

/// Protected whatever the policy file says. Patches may never edit these.
const ALWAYS_PROTECTED: &[&str] = &[
    ".git",
    ".git/**",
    ".autoresolve",
    ".autoresolve/**",
    "policy.toml",
    "autoresolve_regression",
    "autoresolve_regression/**",
];

const DEFAULT_MAX_FILES: usize = 3;
const DEFAULT_MAX_LINES: usize = 40;

/// Unknown keys are an error, so a typo like `max_changed_line` cannot silently disable a limit.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawPolicy {
    protected_paths: Vec<String>,
    max_files_per_patch: Option<usize>,
    max_changed_lines: Option<usize>,
}

#[derive(Debug)]
pub struct Policy {
    protected: GlobSet,
    pub max_files: usize,
    pub max_changed_lines: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self::from_raw(RawPolicy::default()).expect("built-in policy is valid")
    }
}

/// Lexically clean a model- or plan-supplied path into `a/b/c.py`.
/// Rejects absolute paths and `..`. (Symlinks are a separate check: see `reject_symlinks`.)
pub fn normalize(file: &str) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in Path::new(file).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => bail!("path `{file}` must be relative and inside the repo (no `..` or absolute paths)"),
        }
    }
    if parts.is_empty() {
        bail!("empty path");
    }
    Ok(parts.join("/"))
}

impl Policy {
    fn from_raw(raw: RawPolicy) -> Result<Self> {
        let mut b = GlobSetBuilder::new();
        for pat in ALWAYS_PROTECTED.iter().map(|s| s.to_string()).chain(raw.protected_paths) {
            let g = GlobBuilder::new(&pat)
                .case_insensitive(true)
                .literal_separator(true)
                .build()
                .with_context(|| format!("bad pattern in protected_paths: `{pat}`"))?;
            b.add(g);
        }
        Ok(Self {
            protected: b.build()?,
            max_files: raw.max_files_per_patch.unwrap_or(DEFAULT_MAX_FILES),
            max_changed_lines: raw.max_changed_lines.unwrap_or(DEFAULT_MAX_LINES),
        })
    }

    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawPolicy = toml::from_str(text).context("policy.toml is malformed")?;
        Self::from_raw(raw)
    }

    /// Load `<root>/policy.toml`; built-in defaults when the file does not exist.
    /// A file that exists but is invalid is an error: never fall back to weaker rules.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(POLICY_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "max {} file(s) and {} changed line(s) per patch; .git/, .autoresolve/, policy.toml and saved tests are protected",
            self.max_files, self.max_changed_lines
        )
    }

    pub fn is_protected(&self, file: &str) -> bool {
        normalize(file).map_or(true, |p| self.protected.is_match(&p))
    }

    /// Check one patch (a list of edits). The error text goes back to the fixer as feedback.
    pub fn check_edits(&self, edits: &[Edit]) -> Result<()> {
        if edits.is_empty() {
            bail!("policy: the patch contains no edits");
        }
        let mut files: Vec<String> = Vec::new();
        let mut changed = 0usize;
        for e in edits {
            let p = normalize(&e.file).map_err(|err| anyhow::anyhow!("policy: {err}"))?;
            if self.protected.is_match(&p) {
                bail!("policy: `{p}` is a protected path and may not be edited");
            }
            if !files.contains(&p) {
                files.push(p);
            }
            changed += changed_lines(&e.search, &e.replace);
        }
        if files.len() > self.max_files {
            bail!("policy: the patch touches {} files, the limit is {}", files.len(), self.max_files);
        }
        if changed > self.max_changed_lines {
            bail!("policy: the patch changes {changed} lines, the limit is {}", self.max_changed_lines);
        }
        Ok(())
    }

    /// A saved regression test must be exactly `autoresolve_regression/test_<name>.py`.
    pub fn check_test_path(&self, rel: &str) -> Result<()> {
        let p = normalize(rel).map_err(|e| anyhow::anyhow!("policy: test path: {e}"))?;
        let ok = p.strip_prefix(&format!("{TEST_DIR}/")).is_some_and(|name| {
            name.strip_prefix("test_")
                .and_then(|n| n.strip_suffix(".py"))
                .is_some_and(|stem| !stem.is_empty() && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        });
        if !ok {
            bail!("policy: `{rel}` is not an allowed test path (expected {TEST_DIR}/test_<name>.py)");
        }
        Ok(())
    }

    /// Check every item of a saved plan. Run at apply-plan before anything is written.
    pub fn check_plan(&self, plan: &Plan) -> Result<()> {
        for (i, item) in plan.items.iter().enumerate() {
            self.check_edits(&item.edits)
                .with_context(|| format!("plan item {} ({})", i + 1, item.title))?;
            if let Some((rel, _)) = &item.test {
                self.check_test_path(rel)
                    .with_context(|| format!("plan item {} ({})", i + 1, item.title))?;
            }
        }
        Ok(())
    }
}

/// Lines added plus lines removed when `search` becomes `replace`.
fn changed_lines(search: &str, replace: &str) -> usize {
    use similar::{ChangeTag, TextDiff};
    TextDiff::from_lines(search, replace)
        .iter_all_changes()
        .filter(|c| c.tag() != ChangeTag::Equal)
        .count()
}

/// Refuse a path if any existing component of `root/rel` is a symlink.
/// `canonicalize` already stops reads and edits from escaping the repo; this closes the
/// remaining gap: creating files through a symlinked directory (for example a test dir
/// that points outside the repo).
pub fn reject_symlinks(root: &Path, rel: &str) -> Result<()> {
    let mut cur = root.to_path_buf();
    for part in normalize(rel)?.split('/') {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("policy: `{rel}` passes through a symlink ({})", cur.display())
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break, // the rest does not exist yet
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fix::PlanItem;

    fn edit(file: &str, search: &str, replace: &str) -> Edit {
        Edit { file: file.into(), search: search.into(), replace: replace.into() }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("autoresolve-policy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn normalize_rejects_escapes_and_cleans_paths() {
        assert_eq!(normalize("./a//b/./c.py").unwrap(), "a/b/c.py");
        assert!(normalize("../x.py").is_err());
        assert!(normalize("a/../../x.py").is_err());
        assert!(normalize("/etc/passwd").is_err());
        assert!(normalize("").is_err());
        assert!(normalize(".").is_err());
    }

    #[test]
    fn git_and_policy_and_tests_are_always_protected() {
        let p = Policy::default();
        for f in [".git/config", ".git/hooks/pre-commit", "./.git/HEAD", "policy.toml", ".autoresolve/plan.json",
                  "autoresolve_regression/test_0_x.py", ".GIT/config"] {
            assert!(p.check_edits(&[edit(f, "a", "b")]).is_err(), "{f} should be protected");
        }
        assert!(p.check_edits(&[edit("buggy.py", "a", "b")]).is_ok());
        assert!(p.check_edits(&[edit("src/app.py", "a", "b")]).is_ok());
    }

    #[test]
    fn policy_file_adds_protected_paths_and_limits() {
        let p = Policy::parse(
            "protected_paths = [\"tests/**\", \"**/conftest.py\", \"setup.py\"]\nmax_files_per_patch = 1\nmax_changed_lines = 4\n",
        )
        .unwrap();
        assert!(p.check_edits(&[edit("tests/test_a.py", "a", "b")]).is_err());
        assert!(p.check_edits(&[edit("pkg/sub/conftest.py", "a", "b")]).is_err());
        assert!(p.check_edits(&[edit("setup.py", "a", "b")]).is_err());
        assert!(p.check_edits(&[edit("a.py", "x", "y"), edit("b.py", "x", "y")]).is_err()); // 2 files > 1
        assert!(p.check_edits(&[edit("a.py", "1\n2\n3\n", "a\nb\nc\n")]).is_err()); // 6 changed lines > 4
        assert!(p.check_edits(&[edit("a.py", "x\n", "y\n")]).is_ok()); // 2 changed lines
    }

    #[test]
    fn typos_and_bad_patterns_are_errors_not_silent_defaults() {
        assert!(Policy::parse("max_changed_line = 3\n").is_err()); // unknown key
        assert!(Policy::parse("protected_paths = [\"[\"]\n").is_err()); // bad glob
        assert!(Policy::parse("max_files_per_patch = \"two\"\n").is_err()); // wrong type
        let d = tmp("load");
        assert!(Policy::load(&d).is_ok()); // no file: defaults
        std::fs::write(d.join(POLICY_FILE), "max_changed_line = 3\n").unwrap();
        assert!(Policy::load(&d).is_err()); // broken file: refuse, never fall back
    }

    #[test]
    fn empty_patch_is_rejected() {
        assert!(Policy::default().check_edits(&[]).is_err());
    }

    #[test]
    fn test_paths_are_restricted_to_the_regression_directory() {
        let p = Policy::default();
        assert!(p.check_test_path("autoresolve_regression/test_0_last_item.py").is_ok());
        for bad in ["../evil.py", "/tmp/test_x.py", "autoresolve_regression/../x.py", "src/test_x.py",
                    "autoresolve_regression/sub/test_x.py", "autoresolve_regression/test_.py",
                    "autoresolve_regression/test_a-b.py", "autoresolve_regression/helper.py",
                    "autoresolve_regression/test_x.sh"] {
            assert!(p.check_test_path(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn a_tampered_plan_is_caught_before_anything_is_written() {
        let item = |edits: Vec<Edit>, test: Option<(String, String)>| PlanItem {
            title: "t".into(),
            summary: String::new(),
            proven: false,
            edits,
            test,
        };
        let p = Policy::default();
        assert!(p.check_plan(&Plan { items: vec![item(vec![edit("a.py", "x", "y")], None)] }).is_ok());
        let git = Plan { items: vec![item(vec![edit(".git/hooks/post-commit", "x", "y")], None)] };
        assert!(p.check_plan(&git).is_err());
        let evil_test = Plan {
            items: vec![item(vec![edit("a.py", "x", "y")], Some(("../../.bashrc".into(), "x".into())))],
        };
        assert!(p.check_plan(&evil_test).is_err());
    }

    #[test]
    fn symlinked_directories_are_refused() {
        let d = tmp("symlink");
        let outside = tmp("outside");
        std::os::unix::fs::symlink(&outside, d.join("autoresolve_regression")).unwrap();
        assert!(reject_symlinks(&d, "autoresolve_regression/test_x.py").is_err());
        std::fs::create_dir_all(d.join("pkg")).unwrap();
        std::fs::write(d.join("pkg/a.py"), "x\n").unwrap();
        assert!(reject_symlinks(&d, "pkg/a.py").is_ok());
        assert!(reject_symlinks(&d, "pkg/not_created_yet/b.py").is_ok());
    }
}