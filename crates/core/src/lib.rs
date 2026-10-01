pub mod llm;
pub mod agent;
pub mod graph;
pub mod detectors;
use anyhow::{Context, Result};
use tree_sitter::{Node, Parser, Tree};

pub fn parse_python(source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .context("failed to load Python grammar")?;
    parser.parse(source, None).context("parse failed")
}

/// Pretty-print the named nodes of the syntax tree.
pub fn dump(node: Node, source: &str, depth: usize, out: &mut String) {
    let mut next_depth = depth;
    if node.is_named() {
        let leaf = if node.child_count() == 0 {
            format!(" {:?}", &source[node.byte_range()])
        } else {
            String::new()
        };
        out.push_str(&format!("{}{}{}\n", "  ".repeat(depth), node.kind(), leaf));
        next_depth += 1;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        dump(child, source, next_depth, out);
    }
}