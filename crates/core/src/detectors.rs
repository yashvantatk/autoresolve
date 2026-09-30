use anyhow::Result;
use std::path::{Path, PathBuf};
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Query, QueryCursor};

#[derive(Debug, Clone)]
pub struct Finding {
    pub rule: &'static str,
    pub message: String,
    pub file: PathBuf,
    pub line: usize, // 1-based
    pub col: usize,  // 1-based
}

// A tree-sitter query: match a default parameter whose value is a list, dict or set.
const MUTABLE_DEFAULT: &str = r#"
[
  (default_parameter
    name: (identifier) @param
    value: [(list) (dictionary) (set)] @bad)
  (typed_default_parameter
    name: (identifier) @param
    value: [(list) (dictionary) (set)] @bad)
]
"#;

pub fn scan_python(path: &Path, source: &str) -> Result<Vec<Finding>> {
    let tree = crate::parse_python(source)?;
    let lang: Language = tree_sitter_python::LANGUAGE.into();
    let query = Query::new(&lang, MUTABLE_DEFAULT)?;
    let param_idx = query.capture_index_for_name("param").unwrap();
    let bad_idx = query.capture_index_for_name("bad").unwrap();

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut findings = Vec::new();

    while let Some(m) = matches.next() {
        let param = m.nodes_for_capture_index(param_idx).next().unwrap();
        let bad = m.nodes_for_capture_index(bad_idx).next().unwrap();
        let pos = bad.start_position();
        findings.push(Finding {
            rule: "PY001-mutable-default",
            message: format!(
                "parameter `{}` has a mutable default ({}); it is shared across calls",
                &source[param.byte_range()],
                bad.kind()
            ),
            file: path.to_path_buf(),
            line: pos.row + 1,
            col: pos.column + 1,
        });
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_mutable_defaults() {
        let src = "def f(a, b=[], c: dict = {}, d=None):\n    pass\n";
        let f = scan_python(Path::new("t.py"), src).unwrap();
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].line, 1);
    }

    #[test]
    fn ignores_safe_defaults() {
        let src = "def f(a=None, b=(), c=0):\n    pass\n";
        assert!(scan_python(Path::new("t.py"), src).unwrap().is_empty());
    }
}