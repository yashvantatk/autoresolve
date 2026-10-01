use crate::detectors;
use crate::graph::Graph;
use crate::llm::{Message, Provider, ToolCall, ToolSpec};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const SYSTEM: &str = "You are a senior code reviewer working inside a repository. \
Investigate with the tools before drawing conclusions: list symbols, read the code, \
check callers and callees, and run static_findings. Treat static_findings as hints from \
a deterministic scanner, not proof. Report only issues you verified by reading the code. \
For each issue give: severity (high/medium/low), file:line, what is wrong, why it matters, \
and a concrete fix. If you find nothing wrong, say so. Finish with a short markdown report.";

pub struct Tools<'a> {
    graph: &'a Graph,
    root: PathBuf,
}

fn num(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_f64().map(|f| f as u64))
}

impl<'a> Tools<'a> {
    pub fn new(graph: &'a Graph, root: &Path) -> Result<Self> {
        Ok(Self { graph, root: root.canonicalize().context("bad repo root")? })
    }

    pub fn specs() -> Vec<ToolSpec> {
        let name_arg = |desc: &str| {
            json!({"type": "object",
                   "properties": {"name": {"type": "string", "description": desc}},
                   "required": ["name"]})
        };
        vec![
            ToolSpec {
                name: "list_symbols",
                description: "List every function, class and method in the repo as 'file:start-end kind qualname'.",
                parameters: json!({"type": "object", "properties": {}}),
            },
            ToolSpec {
                name: "get_callers",
                description: "Find every place that calls the given function or method name.",
                parameters: name_arg("Function or method name, e.g. validate"),
            },
            ToolSpec {
                name: "get_callees",
                description: "List the functions that the given function calls.",
                parameters: name_arg("Function name or qualified name, e.g. Cart.add"),
            },
            ToolSpec {
                name: "read_lines",
                description: "Read a line range (max 200 lines) from a file in the repo, with line numbers.",
                parameters: json!({"type": "object",
                    "properties": {
                        "file": {"type": "string"},
                        "start": {"type": "integer"},
                        "end": {"type": "integer"}},
                    "required": ["file", "start", "end"]}),
            },
            ToolSpec {
                name: "static_findings",
                description: "Run the deterministic AST anti-pattern detectors on one Python file.",
                parameters: json!({"type": "object",
                    "properties": {"file": {"type": "string"}},
                    "required": ["file"]}),
            },
        ]
    }

    /// Resolve a model-supplied path and refuse anything outside the repo root.
    fn resolve(&self, file: &str) -> Result<PathBuf> {
        let p = self.root.join(file).canonicalize().with_context(|| format!("no such file: {file}"))?;
        if !p.starts_with(&self.root) {
            bail!("path escapes the repository root");
        }
        Ok(p)
    }

    /// Tool errors go back to the model as data so it can recover.
    pub fn call(&self, c: &ToolCall) -> Value {
        match self.dispatch(c) {
            Ok(v) => v,
            Err(e) => json!({"error": e.to_string()}),
        }
    }

    fn dispatch(&self, c: &ToolCall) -> Result<Value> {
        let arg = |k: &str| {
            c.args[k]
                .as_str()
                .map(str::to_owned)
                .with_context(|| format!("missing string argument `{k}`"))
        };
        match c.name.as_str() {
            "list_symbols" => {
                let rows = self.graph.symbols()?;
                Ok(json!(rows
                    .iter()
                    .take(300)
                    .map(|(f, k, q, s, e)| format!("{f}:{s}-{e} {k} {q}"))
                    .collect::<Vec<_>>()))
            }
            "get_callers" => Ok(json!(self
                .graph
                .callers(&arg("name")?)?
                .iter()
                .map(|(caller, f, l)| format!("{caller} at {f}:{l}"))
                .collect::<Vec<_>>())),
            "get_callees" => Ok(json!(self.graph.callees(&arg("name")?)?)),
            "read_lines" => {
                let path = self.resolve(&arg("file")?)?;
                let start = num(&c.args["start"]).unwrap_or(1).max(1) as usize;
                let end = (num(&c.args["end"]).unwrap_or(start as u64 + 60) as usize).min(start + 199);
                let text = std::fs::read_to_string(&path)?;
                let lines: Vec<String> = text
                    .lines()
                    .enumerate()
                    .filter(|(i, _)| i + 1 >= start && i + 1 <= end)
                    .map(|(i, l)| format!("{:>4} | {l}", i + 1))
                    .collect();
                Ok(json!(lines.join("\n")))
            }
            "static_findings" => {
                let path = self.resolve(&arg("file")?)?;
                let src = std::fs::read_to_string(&path)?;
                Ok(serde_json::to_value(detectors::scan_python(&path, &src)?)?)
            }
            other => bail!("unknown tool `{other}`"),
        }
    }
}

/// The agent loop: ask the model, run the tools it requests, repeat until it answers.
pub async fn review(provider: &dyn Provider, tools: &Tools<'_>, target: &str, max_steps: usize) -> Result<String> {
    let specs = Tools::specs();
    let mut history = vec![Message::User(format!(
        "Review `{target}` for bugs, security problems and anti-patterns. \
         Use the tools to look at callers and related code where it matters."
    ))];
    for step in 1..=max_steps {
        let turn = provider.complete(SYSTEM, &history, &specs).await?;
        let calls = turn.calls.clone();
        let text = turn.text.clone();
        history.push(Message::Model(turn));
        if calls.is_empty() {
            return Ok(text);
        }
        let mut results = Vec::new();
        for c in &calls {
            eprintln!("[step {step}] {}({})", c.name, c.args);
            results.push((c.name.clone(), tools.call(c)));
        }
        history.push(Message::ToolResults(results));
    }
    bail!("agent hit the step limit ({max_steps}) without a final answer")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ModelTurn;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Fake model: asks for one tool, then answers. Lets us test the loop with no API key.
    struct Mock(Mutex<usize>);

    #[async_trait]
    impl Provider for Mock {
        async fn complete(&self, _s: &str, history: &[Message], _t: &[ToolSpec]) -> Result<ModelTurn> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Ok(ModelTurn {
                    text: String::new(),
                    calls: vec![ToolCall { name: "list_symbols".into(), args: json!({}) }],
                    raw: json!({}),
                })
            } else {
                assert!(matches!(history.last(), Some(Message::ToolResults(_))));
                Ok(ModelTurn { text: "done".into(), calls: vec![], raw: json!({}) })
            }
        }
    }

    #[tokio::test]
    async fn loop_runs_tools_then_answers() {
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();
        let out = review(&Mock(Mutex::new(0)), &tools, "x.py", 5).await.unwrap();
        assert_eq!(out, "done");
    }

    #[test]
    fn file_reader_refuses_paths_outside_root() {
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();
        let out = tools.call(&ToolCall { name: "read_lines".into(), args: json!({"file": "/etc/passwd", "start": 1, "end": 5}) });
        assert!(out["error"].as_str().unwrap().contains("escapes"));
    }
}