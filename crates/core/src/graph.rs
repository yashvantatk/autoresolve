use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::Path;
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub qualname: String, // e.g. "Cart.add"
    pub kind: &'static str, // "class" | "function" | "method"
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone)]
pub struct CallInfo {
    pub caller: String, // qualname of the enclosing symbol, or "<module>"
    pub callee: String, // called name (for `a.b()` this is "b")
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct FileGraph {
    pub symbols: Vec<SymbolInfo>,
    pub calls: Vec<CallInfo>,
}

type Scope = Vec<(String, &'static str)>;

fn qual(scope: &Scope) -> String {
    scope.iter().map(|s| s.0.as_str()).collect::<Vec<_>>().join(".")
}

pub fn extract(src: &str, tree: &Tree) -> FileGraph {
    let mut out = FileGraph::default();
    let mut scope: Scope = Vec::new();
    walk(tree.root_node(), src, &mut scope, &mut out);
    out
}

fn recurse(node: Node, src: &str, scope: &mut Scope, out: &mut FileGraph) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, scope, out);
    }
}

fn walk(node: Node, src: &str, scope: &mut Scope, out: &mut FileGraph) {
    match node.kind() {
        "function_definition" | "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = src[name_node.byte_range()].to_string();
                let kind = if node.kind() == "class_definition" {
                    "class"
                } else if scope.last().map_or(false, |s| s.1 == "class") {
                    "method"
                } else {
                    "function"
                };
                scope.push((name.clone(), kind));
                out.symbols.push(SymbolInfo {
                    name,
                    qualname: qual(scope),
                    kind,
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                });
                recurse(node, src, scope, out);
                scope.pop();
                return;
            }
        }
        "call" => {
            if let Some(f) = node.child_by_field_name("function") {
                let callee = match f.kind() {
                    "identifier" => Some(&src[f.byte_range()]),
                    "attribute" => f
                        .child_by_field_name("attribute")
                        .map(|a| &src[a.byte_range()]),
                    _ => None,
                };
                if let Some(callee) = callee {
                    let caller = if scope.is_empty() {
                        "<module>".to_string()
                    } else {
                        qual(scope)
                    };
                    out.calls.push(CallInfo {
                        caller,
                        callee: callee.to_string(),
                        line: node.start_position().row + 1,
                    });
                }
            }
        }
        _ => {}
    }
    recurse(node, src, scope, out);
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS files(
    id INTEGER PRIMARY KEY,
    path TEXT UNIQUE NOT NULL
);
CREATE TABLE IF NOT EXISTS symbols(
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id),
    name TEXT NOT NULL,
    qualname TEXT NOT NULL,
    kind TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS calls(
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id),
    caller TEXT NOT NULL,
    callee TEXT NOT NULL,
    line INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_calls_callee ON calls(callee);
CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
";

pub struct Graph {
    conn: Connection,
}

impl Graph {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Wipe the graph and store `files` in a single transaction.
    pub fn replace_all(&mut self, files: &[(String, FileGraph)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch("DELETE FROM calls; DELETE FROM symbols; DELETE FROM files;")?;
        for (path, fg) in files {
            tx.execute("INSERT INTO files(path) VALUES (?1)", params![path])?;
            let fid = tx.last_insert_rowid();
            for s in &fg.symbols {
                tx.execute(
                    "INSERT INTO symbols(file_id,name,qualname,kind,start_line,end_line)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![fid, s.name, s.qualname, s.kind, s.start_line as i64, s.end_line as i64],
                )?;
            }
            for c in &fg.calls {
                tx.execute(
                    "INSERT INTO calls(file_id,caller,callee,line) VALUES (?1,?2,?3,?4)",
                    params![fid, c.caller, c.callee, c.line as i64],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Who calls `name`? -> (caller, file, line)
    pub fn callers(&self, name: &str) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.caller, f.path, c.line
             FROM calls c JOIN files f ON f.id = c.file_id
             WHERE c.callee = ?1 ORDER BY f.path, c.line",
        )?;
        let rows = stmt.query_map(params![name], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// What does `name` call? Accepts "add" or "Cart.add".
    pub fn callees(&self, name: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT callee FROM calls
             WHERE caller = ?1 OR caller LIKE '%.' || ?1
             ORDER BY callee",
        )?;
        let rows = stmt.query_map(params![name], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// All symbols -> (file, kind, qualname, start, end)
    pub fn symbols(&self) -> Result<Vec<(String, String, String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.path, s.kind, s.qualname, s.start_line, s.end_line
             FROM symbols s JOIN files f ON f.id = s.file_id
             ORDER BY f.path, s.start_line",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_symbols_and_calls() {
        let src = "class A:\n    def m(self):\n        helper()\n\ndef helper():\n    pass\n\ndef main():\n    A().m()\n";
        let tree = crate::parse_python(src).unwrap();
        let g = extract(src, &tree);
        let names: Vec<_> = g.symbols.iter().map(|s| s.qualname.as_str()).collect();
        assert_eq!(names, ["A", "A.m", "helper", "main"]);
        assert!(g.calls.iter().any(|c| c.caller == "A.m" && c.callee == "helper"));
        assert!(g.calls.iter().any(|c| c.caller == "main" && c.callee == "m"));
        assert!(g.calls.iter().any(|c| c.caller == "main" && c.callee == "A"));
    }

    #[test]
    fn roundtrip_through_sqlite() {
        let src = "def a():\n    b()\n\ndef b():\n    pass\n";
        let tree = crate::parse_python(src).unwrap();
        let files = vec![("x.py".to_string(), extract(src, &tree))];
        let mut g = Graph::open(Path::new(":memory:")).unwrap();
        g.replace_all(&files).unwrap();
        let callers = g.callers("b").unwrap();
        assert_eq!(callers, vec![("a".to_string(), "x.py".to_string(), 2)]);
    }
}