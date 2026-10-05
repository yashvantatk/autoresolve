//! `autoresolve mcp`: a Model Context Protocol server over stdio (JSON-RPC 2.0, one message per line).
//! It exposes READ-ONLY tools: scan, symbols, callers, callees, events_summary, and review (which calls
//! your model). It never exposes fix or apply-plan, and every path is confined to the directory the
//! server was started in. stdout carries only protocol messages; logs go to stderr.

use anyhow::{bail, Result};
use autoresolve_core::agent::Tools;
use autoresolve_core::graph::Graph;
use autoresolve_core::{detectors, events, llm, report, review};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const PROTOCOL: &str = "2025-06-18";

fn tool_specs() -> Value {
    let path = json!({"type": "string", "description": "Directory to work in, relative to the server's start directory (default \".\")"});
    json!([
        {"name": "scan", "description": "AST rules over Python files (mutable defaults, bare except, == None). No model calls.",
         "inputSchema": {"type": "object", "properties": {"path": path}}},
        {"name": "symbols", "description": "List functions and classes of the Python code under a directory (repo graph).",
         "inputSchema": {"type": "object", "properties": {"path": path}}},
        {"name": "callers", "description": "Who calls this function? (matched by name)",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "path": path}, "required": ["name"]}},
        {"name": "callees", "description": "What does this function call? (matched by name)",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "path": path}, "required": ["name"]}},
        {"name": "events_summary", "description": "Per-role summary of the latest recorded AutoResolve run. No model calls.",
         "inputSchema": {"type": "object", "properties": {"path": path}}},
        {"name": "review", "description": "Agentic review of one Python file by reviewer and skeptic agents. CALLS YOUR MODEL (needs GEMINI_API_KEY).",
         "inputSchema": {"type": "object", "properties": {"file": {"type": "string"}, "path": path}, "required": ["file"]}}
    ])
}

/// Resolve `p` under `base`; anything that escapes `base` is refused.
fn confine(base: &Path, p: &str) -> Result<PathBuf> {
    let full = base.join(p).canonicalize()?;
    if !full.starts_with(base) {
        bail!("path `{p}` is outside the directory the server was started in");
    }
    Ok(full)
}

fn text_arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(|v| v.as_str())
}

fn db_for(root: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(root.join(".autoresolve"))?;
    Ok(root.join(".autoresolve").join("graph.db"))
}

async fn call_tool(base: &Path, name: &str, args: &Value) -> Result<String> {
    let root = confine(base, text_arg(args, "path").unwrap_or("."))?;
    match name {
        "scan" => {
            let mut findings = Vec::new();
            for p in crate::python_files(&root)? {
                let Ok(src) = std::fs::read_to_string(&p) else { continue };
                findings.extend(detectors::scan_python(&p, &src)?);
            }
            Ok(serde_json::to_string_pretty(&findings)?)
        }
        "symbols" | "callers" | "callees" => {
            let db = db_for(&root)?;
            crate::index_repo(&root, &db)?;
            let g = Graph::open(&db)?;
            let out = match name {
                "symbols" => {
                    let rows: Vec<Value> = g
                        .symbols()?
                        .into_iter()
                        .take(500)
                        .map(|(file, kind, qualname, start, end)| json!({"file": file, "kind": kind, "name": qualname, "start": start, "end": end}))
                        .collect();
                    json!(rows)
                }
                "callers" => {
                    let n = text_arg(args, "name").ok_or_else(|| anyhow::anyhow!("`name` is required"))?;
                    let rows: Vec<Value> = g.callers(n)?.into_iter().map(|(c, f, l)| json!({"caller": c, "file": f, "line": l})).collect();
                    json!(rows)
                }
                _ => {
                    let n = text_arg(args, "name").ok_or_else(|| anyhow::anyhow!("`name` is required"))?;
                    json!(g.callees(n)?)
                }
            };
            Ok(serde_json::to_string_pretty(&out)?)
        }
        "events_summary" => {
            let all = events::read_events(&root.join(".autoresolve").join(events::LOG_FILE), None)?;
            let Some(last) = all.last().map(|e| e.run.clone()) else { return Ok("no recorded runs".into()) };
            let run: Vec<_> = all.into_iter().filter(|e| e.run == last).collect();
            Ok(events::summarize(&run))
        }
        "review" => {
            let file = text_arg(args, "file").ok_or_else(|| anyhow::anyhow!("`file` is required"))?;
            confine(&root, file)?; // the file must live under the root too
            let provider = llm::provider_from_env(false)?;
            events::init(&root.join(".autoresolve").join(events::LOG_FILE), &events::new_run_id())?;
            events::emit("run_start", events::run_config("review", file));
            let db = db_for(&root)?;
            crate::index_repo(&root, &db)?;
            let graph = Graph::open(&db)?;
            let tools = Tools::new(&graph, &root)?;
            let opts = review::ReviewOpts { specialists: 0, votes: 1, min_votes: None };
            let judged = review::review_with(&provider, &tools, file, 12, &opts).await?;
            Ok(report::markdown_from_review(&judged))
        }
        other => bail!("unknown tool `{other}`"),
    }
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Answer one JSON-RPC message. Notifications (no id) get no answer.
async fn handle(base: &Path, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg["method"].as_str().unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => ok(
            id,
            json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "autoresolve", "version": env!("CARGO_PKG_VERSION")}
            }),
        ),
        "ping" => ok(id, json!({})),
        "tools/list" => ok(id, json!({"tools": tool_specs()})),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            match call_tool(base, name, &params["arguments"]).await {
                Ok(text) => ok(id, json!({"content": [{"type": "text", "text": text}]})),
                Err(e) => ok(id, json!({"content": [{"type": "text", "text": format!("error: {e}")}], "isError": true})),
            }
        }
        _ => err(id, -32601, "method not found"),
    })
}

pub async fn serve() -> Result<()> {
    let base = std::env::current_dir()?.canonicalize()?;
    eprintln!("[mcp] autoresolve server on stdio, confined to {}", base.display());
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&base, &msg).await,
            Err(_) => Some(err(Value::Null, -32700, "parse error")),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("autoresolve-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[tokio::test]
    async fn the_protocol_basics_work() {
        let d = tmp("proto");
        let init = handle(&d, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).await.unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "autoresolve");
        let list = handle(&d, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await.unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["scan", "symbols", "callers", "callees", "events_summary", "review"]);
        assert!(!names.contains(&"fix") && !names.contains(&"apply_plan"));
        // a notification gets no answer; an unknown method gets an error
        assert!(handle(&d, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await.is_none());
        let bad = handle(&d, &json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})).await.unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn scan_finds_a_mutable_default_and_paths_cannot_escape() {
        let d = tmp("scan");
        std::fs::write(d.join("a.py"), "def f(x=[]):\n    return x\n").unwrap();
        let call = |args: Value| json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "scan", "arguments": args}});
        let r = handle(&d, &call(json!({}))).await.unwrap();
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("PY001"));
        let esc = handle(&d, &call(json!({"path": ".."}))).await.unwrap();
        assert_eq!(esc["result"]["isError"], true);
    }
}
