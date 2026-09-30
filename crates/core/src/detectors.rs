use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Query, QueryCursor, Tree};

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub message: String,
    pub file: PathBuf,
    pub line: usize, // 1-based
    pub col: usize,  // 1-based
}

/// One anti-pattern detector. Add a new rule = implement this trait.
pub trait Rule {
    fn id(&self) -> &'static str;
    fn check(&self, path: &Path, src: &str, tree: &Tree) -> Vec<Finding>;
}

fn finding(rule: &'static str, path: &Path, node: Node, message: String) -> Finding {
    let pos = node.start_position();
    Finding {
        rule,
        message,
        file: path.to_path_buf(),
        line: pos.row + 1,
        col: pos.column + 1,
    }
}

/// Run a tree-sitter query and return every node bound to `cap`.
fn capture_nodes<'t>(tree: &'t Tree, src: &str, query_src: &str, cap: &str) -> Vec<Node<'t>> {
    let lang: Language = tree_sitter_python::LANGUAGE.into();
    let query = Query::new(&lang, query_src).expect("invalid query");
    let idx = query.capture_index_for_name(cap).expect("missing capture");
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());
    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        out.extend(m.nodes_for_capture_index(idx));
    }
    out
}

// ---------- PY001: mutable default argument ----------
pub struct MutableDefault;

impl Rule for MutableDefault {
    fn id(&self) -> &'static str {
        "PY001-mutable-default"
    }
    fn check(&self, path: &Path, src: &str, tree: &Tree) -> Vec<Finding> {
        let q = r#"
        [
          (default_parameter value: [(list) (dictionary) (set)] @bad)
          (typed_default_parameter value: [(list) (dictionary) (set)] @bad)
        ]"#;
        capture_nodes(tree, src, q, "bad")
            .into_iter()
            .map(|bad| {
                let name = bad
                    .parent()
                    .and_then(|p| p.child_by_field_name("name"))
                    .map(|n| &src[n.byte_range()])
                    .unwrap_or("?");
                finding(
                    self.id(),
                    path,
                    bad,
                    format!("parameter `{name}` has a mutable default ({}); it is shared across calls", bad.kind()),
                )
            })
            .collect()
    }
}

// ---------- PY002: bare except ----------
pub struct BareExcept;

impl Rule for BareExcept {
    fn id(&self) -> &'static str {
        "PY002-bare-except"
    }
    fn check(&self, path: &Path, src: &str, tree: &Tree) -> Vec<Finding> {
        capture_nodes(tree, src, "(except_clause) @bad", "bad")
            .into_iter()
            // a bare `except:` has only its block as a named child
            .filter(|n| n.named_child_count() == 1)
            .map(|n| {
                finding(
                    self.id(),
                    path,
                    n,
                    "bare `except:` also catches KeyboardInterrupt/SystemExit; catch a specific exception".into(),
                )
            })
            .collect()
    }
}

// ---------- PY003: comparison to None with ==/!= ----------
pub struct NoneComparison;

impl Rule for NoneComparison {
    fn id(&self) -> &'static str {
        "PY003-none-comparison"
    }
    fn check(&self, path: &Path, src: &str, tree: &Tree) -> Vec<Finding> {
        capture_nodes(tree, src, "(comparison_operator) @bad", "bad")
            .into_iter()
            .filter(|n| {
                let mut c = n.walk();
                let kinds: Vec<&str> = n.children(&mut c).map(|k| k.kind()).collect();
                kinds.contains(&"none") && (kinds.contains(&"==") || kinds.contains(&"!="))
            })
            .map(|n| {
                finding(self.id(), path, n, "compare to None with `is` / `is not`, not `==` / `!=`".into())
            })
            .collect()
    }
}

pub fn all_rules() -> Vec<Box<dyn Rule>> {
    vec![Box::new(MutableDefault), Box::new(BareExcept), Box::new(NoneComparison)]
}

/// Parse once, run every rule, return findings sorted by position.
pub fn scan_python(path: &Path, source: &str) -> Result<Vec<Finding>> {
    let tree = crate::parse_python(source)?;
    let mut out: Vec<Finding> = all_rules()
        .iter()
        .flat_map(|r| r.check(path, source, &tree))
        .collect();
    out.sort_by_key(|f| (f.line, f.col));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(src: &str) -> Vec<Finding> {
        scan_python(Path::new("t.py"), src).unwrap()
    }

    #[test]
    fn flags_mutable_defaults() {
        let f = scan("def f(a, b=[], c: dict = {}, d=None):\n    pass\n");
        assert_eq!(f.iter().filter(|x| x.rule.starts_with("PY001")).count(), 2);
    }

    #[test]
    fn ignores_safe_defaults() {
        assert!(scan("def f(a=None, b=(), c=0):\n    pass\n").is_empty());
    }

    #[test]
    fn flags_bare_except_only() {
        let bare = scan("try:\n    x()\nexcept:\n    pass\n");
        assert!(bare.iter().any(|x| x.rule.starts_with("PY002")));
        let typed = scan("try:\n    x()\nexcept ValueError:\n    pass\n");
        assert!(typed.iter().all(|x| !x.rule.starts_with("PY002")));
    }

    #[test]
    fn flags_none_comparison() {
        assert!(scan("if x == None:\n    pass\n").iter().any(|x| x.rule.starts_with("PY003")));
        assert!(scan("if x is None:\n    pass\n").is_empty());
    }
}