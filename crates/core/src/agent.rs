use crate::detectors;
use crate::graph::Graph;
use crate::llm::{Message, Provider, ToolCall, ToolSpec};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

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

    pub fn root(&self) -> &Path {
        &self.root
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

/// Generic agent loop. Runs until the model calls the `terminal` tool,
/// then returns that call's arguments (structured output).
/// The terminal tool is refused until the model has used at least one other tool, and a model
/// that keeps answering in prose is switched to schema-constrained output where the backend supports it.
pub async fn run_agent(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    system: &str,
    task: &str,
    specs: Vec<ToolSpec>,
    terminal: &str,
    max_steps: usize,
) -> Result<Value> {
    let mut history = vec![Message::User(task.to_string())];
    let mut seen_calls = std::collections::HashSet::new();
    let mut used_tool = false; // the model must look at the code before it may submit
    let mut prose_streak = 0; // consecutive answers in prose instead of a tool call
    for step in 1..=max_steps {
        let turn = provider.complete(system, &history, &specs).await?;
        let calls = turn.calls.clone();
        history.push(Message::Model(turn));

        if calls.is_empty() {
            prose_streak += 1;
            // a model that never touches a tool cannot be forced into structured output (it has not
            // seen the code), so tell it to look, and stop early instead of burning every step
            if !used_tool {
                if prose_streak >= 4 {
                    bail!("the model answered in prose {prose_streak} times without using any tool; giving up on this attempt");
                }
                eprintln!("[step {step}] model answered in prose before reading any code; telling it to use a tool");
                history.push(Message::User(format!(
                    "You have not looked at the code yet. Call `list_symbols` or `read_lines` now (a tool call, not prose), \
                     then finish by calling `{terminal}`."
                )));
                continue;
            }
            if prose_streak >= 2 && used_tool {
                if let Some(spec) = specs.iter().find(|s| s.name == terminal) {
                    match provider.complete_json(system, &history, &spec.parameters).await {
                        Ok(v) => {
                            eprintln!("[step {step}] model kept answering in prose; forced structured output for `{terminal}`");
                            return Ok(v);
                        }
                        Err(e) => eprintln!("[step {step}] structured output unavailable ({e})"),
                    }
                }
            }
            eprintln!("[step {step}] model answered in prose; asking it to call `{terminal}`");
            history.push(Message::User(format!(
                "Do not answer in prose. Finish by calling the `{terminal}` tool."
            )));
            continue;
        }
        prose_streak = 0;

        let used_before = used_tool;
        let mut done: Option<Value> = None;
        let mut results = Vec::new();
        for c in &calls {
            if c.name == terminal {
                if used_before {
                    done = Some(c.args.clone());
                } else {
                    eprintln!("[step {step}] `{terminal}` called before reading any code; sending the model back to investigate");
                    results.push((
                        c.name.clone(),
                        json!({"error": format!("Too early: you have not looked at any code yet. Use list_symbols and read_lines first, then call `{terminal}`.")}),
                    ));
                }
                continue;
            }
            used_tool = true;
            let result = if seen_calls.insert(format!("{}:{}", c.name, c.args)) {
                eprintln!("[step {step}] {}({})", c.name, c.args);
                tools.call(c)
            } else {
                eprintln!("[step {step}] repeated call {}; telling the model to wrap up", c.name);
                json!({"error": format!("You already made this exact call and have its result above. Do not repeat it; call `{terminal}` now.")})
            };
            results.push((c.name.clone(), result));
        }
        if let Some(args) = done {
            return Ok(args);
        }
        history.push(Message::ToolResults(results));
    }
    bail!("agent hit the step limit ({max_steps}) without calling `{terminal}`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ModelTurn;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Fake model: uses one tool, then calls the terminal tool. No API key needed.
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
                Ok(ModelTurn {
                    text: String::new(),
                    calls: vec![ToolCall { name: "submit".into(), args: json!({"ok": true}) }],
                    raw: json!({}),
                })
            }
        }
    }

    #[tokio::test]
    async fn loop_runs_tools_then_returns_terminal_args() {
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();
        let out = run_agent(&Mock(Mutex::new(0)), &tools, "sys", "task", vec![], "submit", 5)
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn file_reader_refuses_paths_outside_root() {
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();
        let out = tools.call(&ToolCall {
            name: "read_lines".into(),
            args: json!({"file": "/etc/passwd", "start": 1, "end": 5}),
        });
        assert!(out["error"].as_str().unwrap().contains("escapes"));
    }

    /// Fake model that repeats one call; the loop must not execute the repeat.
    struct Repeater(Mutex<usize>);

    #[async_trait]
    impl Provider for Repeater {
        async fn complete(&self, _s: &str, history: &[Message], _t: &[ToolSpec]) -> Result<ModelTurn> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            if *n <= 2 {
                return Ok(ModelTurn {
                    text: String::new(),
                    calls: vec![ToolCall { name: "list_symbols".into(), args: json!({}) }],
                    raw: json!({}),
                });
            }
            match history.last() {
                Some(Message::ToolResults(rs)) => {
                    assert!(rs[0].1["error"].as_str().unwrap().contains("already made"))
                }
                _ => panic!("expected tool results"),
            }
            Ok(ModelTurn {
                text: String::new(),
                calls: vec![ToolCall { name: "submit".into(), args: json!({"ok": true}) }],
                raw: json!({}),
            })
        }
    }

    #[tokio::test]
    async fn repeated_calls_are_not_executed_twice() {
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();
        let out = run_agent(&Repeater(Mutex::new(0)), &tools, "sys", "task", vec![], "submit", 5)
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    /// Lazy fake model: tries to submit at once, then investigates after being sent back.
    struct Lazy(Mutex<usize>);

    #[async_trait]
    impl Provider for Lazy {
        async fn complete(&self, _s: &str, history: &[Message], _t: &[ToolSpec]) -> Result<ModelTurn> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            let call = |name: &str| ModelTurn {
                text: String::new(),
                calls: vec![ToolCall { name: name.into(), args: json!({"n": 1}) }],
                raw: json!({}),
            };
            match *n {
                1 => Ok(call("submit")),
                2 => {
                    match history.last() {
                        Some(Message::ToolResults(rs)) => {
                            assert!(rs[0].1["error"].as_str().unwrap().contains("Too early"))
                        }
                        _ => panic!("expected the rejection"),
                    }
                    Ok(call("list_symbols"))
                }
                _ => Ok(call("submit")),
            }
        }
    }

    #[tokio::test]
    async fn submitting_before_investigating_is_rejected() {
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();
        let out = run_agent(&Lazy(Mutex::new(0)), &tools, "sys", "task", vec![], "submit", 6)
            .await
            .unwrap();
        assert_eq!(out["n"], 1);
    }

    /// Chatty fake model: uses one tool, then keeps answering in prose. Supports structured output.
    struct Chatty(Mutex<usize>);

    #[async_trait]
    impl Provider for Chatty {
        async fn complete(&self, _s: &str, _h: &[Message], _t: &[ToolSpec]) -> Result<ModelTurn> {
            let mut n = self.0.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Ok(ModelTurn {
                    text: String::new(),
                    calls: vec![ToolCall { name: "list_symbols".into(), args: json!({}) }],
                    raw: json!({}),
                })
            } else {
                Ok(ModelTurn { text: "I think the bug is real.".into(), calls: vec![], raw: json!({}) })
            }
        }

        async fn complete_json(&self, _s: &str, _h: &[Message], _schema: &Value) -> Result<Value> {
            Ok(json!({"verdict": "confirmed"}))
        }
    }

    #[tokio::test]
    async fn repeated_prose_forces_structured_output() {
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();
        let spec = ToolSpec {
            name: "submit",
            description: "d",
            parameters: json!({"type": "object", "properties": {"verdict": {"type": "string"}}}),
        };
        let out = run_agent(&Chatty(Mutex::new(0)), &tools, "sys", "task", vec![spec], "submit", 8)
            .await
            .unwrap();
        assert_eq!(out["verdict"], "confirmed");
    }

    /// Fake model that only ever writes prose and never calls a tool.
    struct ProseOnly(Mutex<usize>);

    #[async_trait]
    impl Provider for ProseOnly {
        async fn complete(&self, _s: &str, _h: &[Message], _t: &[ToolSpec]) -> Result<ModelTurn> {
            *self.0.lock().unwrap() += 1;
            Ok(ModelTurn { text: "let me think about it".into(), calls: vec![], raw: json!({}) })
        }
    }

    #[tokio::test]
    async fn a_model_that_never_uses_a_tool_is_stopped_early() {
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();
        let model = ProseOnly(Mutex::new(0));
        let err = run_agent(&model, &tools, "sys", "task", vec![], "submit", 12).await.unwrap_err();
        assert!(err.to_string().contains("without using any tool"));
        assert_eq!(*model.0.lock().unwrap(), 4); // not 12
    }
}