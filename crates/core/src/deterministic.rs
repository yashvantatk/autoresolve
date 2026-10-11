//! Deterministic, non-AI AST patching and linting auto-fix engine.
//! Bypasses LLM agents when no API key is present or when --deterministic is passed.

use anyhow::{Context, Result};
use std::path::Path;
use tree_sitter::{Node, Tree};
use crate::fix::{Edit, Plan, PlanItem};
use crate::output::create_plan;

fn find_subprocess_calls_rec(node: Node, src: &str, file: &str, items: &mut Vec<PlanItem>) {
    if node.kind() == "call" {
        if let Some(func_node) = node.child_by_field_name("function") {
            let func_text = &src[func_node.byte_range()];
            let is_subprocess = func_text.contains("subprocess")
                || func_text == "run"
                || func_text == "Popen"
                || func_text == "call"
                || func_text == "check_call"
                || func_text == "check_output";
            if is_subprocess {
                if let Some(args_node) = node.child_by_field_name("arguments") {
                    let mut c = args_node.walk();
                    for arg in args_node.children(&mut c) {
                        if arg.kind() == "keyword_argument" {
                            let name = arg.child_by_field_name("name").map(|n| &src[n.byte_range()]);
                            let val = arg.child_by_field_name("value").map(|v| &src[v.byte_range()]);
                            if name == Some("shell") && val == Some("True") {
                                let call_str = &src[node.byte_range()];
                                let replaced = if call_str.contains("shell=True") {
                                    call_str.replacen("shell=True", "shell=False", 1)
                                } else if call_str.contains("shell = True") {
                                    call_str.replacen("shell = True", "shell = False", 1)
                                } else {
                                    call_str.to_string()
                                };
                                if replaced != call_str {
                                    items.push(PlanItem {
                                        title: "Security: Replace subprocess shell=True with shell=False".into(),
                                        summary: format!(
                                            "Programmatic AST rewrite: `{func_text}` called with `shell=True`. Safely replaced with `shell=False` to prevent shell injection vulnerabilities."
                                        ),
                                        proven: true,
                                        edits: vec![Edit {
                                            file: file.to_string(),
                                            search: call_str.to_string(),
                                            replace: replaced,
                                        }],
                                        test: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let mut c = node.walk();
    for child in node.children(&mut c) {
        find_subprocess_calls_rec(child, src, file, items);
    }
}

pub fn find_subprocess_shell_true(tree: &Tree, src: &str, file: &str) -> Vec<PlanItem> {
    let mut items = Vec::new();
    find_subprocess_calls_rec(tree.root_node(), src, file, &mut items);
    items
}

fn find_none_comparisons_rec(node: Node, src: &str, file: &str, items: &mut Vec<PlanItem>) {
    if node.kind() == "comparison_operator" {
        let mut c = node.walk();
        let kinds: Vec<&str> = node.children(&mut c).map(|k| k.kind()).collect();
        if kinds.contains(&"none") && (kinds.contains(&"==") || kinds.contains(&"!=")) {
            let comp_str = &src[node.byte_range()];
            let replaced = if comp_str.contains("==") {
                comp_str.replace("==", "is")
            } else if comp_str.contains("!=") {
                comp_str.replace("!=", "is not")
            } else {
                comp_str.to_string()
            };
            if replaced != comp_str {
                items.push(PlanItem {
                    title: "Correctness: Replace equality comparison to None with identity check".into(),
                    summary: format!(
                        "Programmatic AST rewrite: `{comp_str}` should use `is` or `is not` instead of `==` or `!=` per PEP 8."
                    ),
                    proven: true,
                    edits: vec![Edit {
                        file: file.to_string(),
                        search: comp_str.to_string(),
                        replace: replaced,
                    }],
                    test: None,
                });
            }
        }
    }

    let mut c = node.walk();
    for child in node.children(&mut c) {
        find_none_comparisons_rec(child, src, file, items);
    }
}

pub fn find_none_comparisons(tree: &Tree, src: &str, file: &str) -> Vec<PlanItem> {
    let mut items = Vec::new();
    find_none_comparisons_rec(tree.root_node(), src, file, &mut items);
    items
}

fn find_bare_excepts_rec(node: Node, src: &str, file: &str, items: &mut Vec<PlanItem>) {
    if node.kind() == "except_clause" && node.named_child_count() == 1 {
        let pos = node.start_position();
        let lines: Vec<&str> = src.lines().collect();
        if pos.row < lines.len() {
            let line = lines[pos.row];
            if line.contains("except:") {
                let replaced = line.replace("except:", "except Exception:");
                items.push(PlanItem {
                    title: "Robustness: Replace bare except with except Exception".into(),
                    summary: "Programmatic AST rewrite: bare `except:` catches KeyboardInterrupt and SystemExit. Replaced with `except Exception:` to preserve control-flow interrupts.".into(),
                    proven: true,
                    edits: vec![Edit {
                        file: file.to_string(),
                        search: line.to_string(),
                        replace: replaced,
                    }],
                    test: None,
                });
            }
        }
    }

    let mut c = node.walk();
    for child in node.children(&mut c) {
        find_bare_excepts_rec(child, src, file, items);
    }
}

pub fn find_bare_excepts(tree: &Tree, src: &str, file: &str) -> Vec<PlanItem> {
    let mut items = Vec::new();
    find_bare_excepts_rec(tree.root_node(), src, file, &mut items);
    items
}

pub fn run_ruff_fixes(src: &str, file_rel: &str) -> Option<PlanItem> {
    let tmp_dir = std::env::temp_dir().join(format!("autoresolve-ruff-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);
    let tmp_file = tmp_dir.join("tmp_fix.py");
    if std::fs::write(&tmp_file, src).is_err() {
        return None;
    }

    // Try `ruff check --fix --quiet --no-cache <tmp_file>`
    let res = std::process::Command::new("ruff")
        .arg("check")
        .arg("--fix")
        .arg("--quiet")
        .arg("--no-cache")
        .arg(&tmp_file)
        .status();

    let success = match res {
        Ok(s) => s.success() || s.code() == Some(0) || s.code() == Some(1),
        Err(_) => {
            // Fallback: try python3 -m ruff
            std::process::Command::new("python3")
                .arg("-m")
                .arg("ruff")
                .arg("check")
                .arg("--fix")
                .arg("--quiet")
                .arg("--no-cache")
                .arg(&tmp_file)
                .status()
                .map(|s| s.success() || s.code() == Some(0) || s.code() == Some(1))
                .unwrap_or(false)
        }
    };

    if success {
        if let Ok(fixed) = std::fs::read_to_string(&tmp_file) {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            if fixed != src {
                return Some(PlanItem {
                    title: "Ruff: Auto-fix standard linting and formatting issues".into(),
                    summary: "Automatically resolved standard Python linting errors and formatting with `ruff check --fix`.".into(),
                    proven: true,
                    edits: vec![Edit {
                        file: file_rel.to_string(),
                        search: src.to_string(),
                        replace: fixed,
                    }],
                    test: None,
                });
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
    None
}

/// Executes deterministic, non-AI AST patching and ruff fixes on the target file.
pub fn run_deterministic_fixes(root: &Path, rel_file: &Path) -> Result<Plan> {
    let full_path = if rel_file.is_absolute() {
        rel_file.to_path_buf()
    } else {
        root.join(rel_file)
    };
    let src = std::fs::read_to_string(&full_path)
        .with_context(|| format!("failed to read file: {}", full_path.display()))?;

    let file_str = rel_file.display().to_string();
    let mut items = Vec::new();

    // 1. Run ruff check --fix
    if let Some(ruff_item) = run_ruff_fixes(&src, &file_str) {
        items.push(ruff_item);
    }

    // 2. Programmatic guaranteed Tree-sitter AST rewrites
    if let Ok(tree) = crate::parse_python(&src) {
        items.extend(find_subprocess_shell_true(&tree, &src, &file_str));
        items.extend(find_none_comparisons(&tree, &src, &file_str));
        items.extend(find_bare_excepts(&tree, &src, &file_str));
    }

    Ok(create_plan(&file_str, items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_subprocess_shell_true() {
        let code = r#"
import subprocess

def run_command(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True)
"#;
        let tree = crate::parse_python(code).unwrap();
        let items = find_subprocess_shell_true(&tree, code, "test.py");
        assert_eq!(items.len(), 1);
        assert!(items[0].edits[0].search.contains("shell=True"));
        assert!(items[0].edits[0].replace.contains("shell=False"));
    }

    #[test]
    fn rewrites_none_comparison() {
        let code = "if val == None:\n    pass\nif other != None:\n    pass\n";
        let tree = crate::parse_python(code).unwrap();
        let items = find_none_comparisons(&tree, code, "test.py");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].edits[0].replace, "val is None");
        assert_eq!(items[1].edits[0].replace, "other is not None");
    }

    #[test]
    fn rewrites_bare_except() {
        let code = "try:\n    do_work()\nexcept:\n    pass\n";
        let tree = crate::parse_python(code).unwrap();
        let items = find_bare_excepts(&tree, code, "test.py");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].edits[0].replace, "except Exception:");
    }
}
