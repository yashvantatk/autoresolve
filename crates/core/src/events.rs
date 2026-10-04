//! Event log: every model turn, tool call, check and outcome of a run is appended to
//! `.autoresolve/events.jsonl`, one JSON object per line. It is the source for per-role
//! cost and timing numbers, and what a replay UI reads later.
//!
//! Logging must never break a run: if the log cannot be opened or written, events are dropped.
//! Never put secrets in an event: only model text, tool arguments/results and check output go in.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::future::Future;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOG_FILE: &str = "events.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub ts_ms: u64,
    pub run: String,
    /// Which agent was acting: reviewer, skeptic, tester, fixer, gate, still_present, or controller.
    pub role: String,
    pub kind: String,
    pub data: Value,
}

struct Sink {
    file: std::fs::File,
    run: String,
    seq: u64,
}

fn sink() -> &'static Mutex<Option<Sink>> {
    static S: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

tokio::task_local! {
    static ROLE: &'static str;
}

/// Run a future with `role` as the acting agent: every event emitted inside is tagged with it.
pub async fn scope<F: Future>(role: &'static str, f: F) -> F::Output {
    ROLE.scope(role, f).await
}

fn current_role() -> String {
    ROLE.try_with(|r| r.to_string()).unwrap_or_else(|_| "controller".into())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn new_run_id() -> String {
    (now_ms() / 1000).to_string()
}

/// Start logging to `path` (created if needed, appended to otherwise) under run id `run`.
pub fn init(path: &Path, run: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Ok(m) = std::fs::symlink_metadata(path) {
        if m.file_type().is_symlink() {
            bail!("refusing to write the event log through a symlink: {}", path.display());
        }
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    *sink().lock().unwrap() = Some(Sink { file, run: run.to_string(), seq: 0 });
    Ok(())
}

/// Append one event. A no-op when logging was never started.
pub fn emit(kind: &str, data: Value) {
    let mut guard = match sink().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(s) = guard.as_mut() else { return };
    s.seq += 1;
    let ev = Event { seq: s.seq, ts_ms: now_ms(), run: s.run.clone(), role: current_role(), kind: kind.into(), data };
    if let Ok(line) = serde_json::to_string(&ev) {
        let _ = writeln!(s.file, "{line}");
        let _ = s.file.flush();
    }
}

/// Shorten long text for the log (on a character boundary).
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

/// Which configuration produced a run (names only, never keys).
pub fn run_config(command: &str, target: &str) -> Value {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    json!({
        "command": command,
        "target": target,
        "provider": env("AUTORESOLVE_PROVIDER"),
        "provider_strong": env("AUTORESOLVE_PROVIDER_STRONG"),
        "model": env("AUTORESOLVE_MODEL"),
        "model_strong": env("AUTORESOLVE_MODEL_STRONG"),
        "ollama_model": env("AUTORESOLVE_OLLAMA_MODEL"),
        "sandbox": env("AUTORESOLVE_SANDBOX"),
        "docker_image": env("AUTORESOLVE_DOCKER_IMAGE"),
    })
}

/// Read events from a log, skipping lines that are not valid events (a run that was killed
/// mid-write leaves a partial last line). `run` keeps only that run.
pub fn read_events(path: &Path, run: Option<&str>) -> Result<Vec<Event>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<Event>(l).ok())
        .filter(|e| run.is_none_or(|r| e.run == r))
        .collect())
}

/// Run ids in the order they first appear, with their event counts.
pub fn runs(events: &[Event]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for e in events {
        match out.iter_mut().find(|(r, _)| *r == e.run) {
            Some((_, n)) => *n += 1,
            None => out.push((e.run.clone(), 1)),
        }
    }
    out
}

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// Per-role table plus the run's outcome, from the events of ONE run.
pub fn summarize(events: &[Event]) -> String {
    if events.is_empty() {
        return "no events".into();
    }
    let mut out = String::new();
    let start = events.iter().find(|e| e.kind == "run_start");
    let end = events.iter().rev().find(|e| e.kind == "run_end");
    let s = |e: Option<&Event>, k: &str| e.and_then(|e| e.data[k].as_str()).unwrap_or("").to_string();
    let wall = match (start, end) {
        (Some(a), Some(b)) => fmt_ms(b.ts_ms.saturating_sub(a.ts_ms)),
        _ => "unfinished".into(),
    };
    out.push_str(&format!(
        "run {}  {} {}  ({})\n",
        events[0].run,
        s(start, "command"),
        s(start, "target"),
        wall
    ));
    if start.is_some() {
        let uses_ollama = s(start, "provider") == "ollama" || s(start, "provider_strong") == "ollama";
        out.push_str(&format!(
            "config: provider={} worker={} model={} worker_model={}{}\n",
            s(start, "provider"),
            s(start, "provider_strong"),
            s(start, "model"),
            s(start, "model_strong"),
            if uses_ollama { format!(" ollama={}", s(start, "ollama_model")) } else { String::new() }
        ));
    }
    // per-role stats, in order of first appearance
    struct Row {
        role: String,
        turns: usize,
        ms: u64,
        tools: usize,
        prose: usize,
    }
    let mut rows: Vec<Row> = Vec::new();
    for e in events {
        if e.kind != "model_turn" && e.kind != "tool_call" {
            continue;
        }
        let i = match rows.iter().position(|r| r.role == e.role) {
            Some(i) => i,
            None => {
                rows.push(Row { role: e.role.clone(), turns: 0, ms: 0, tools: 0, prose: 0 });
                rows.len() - 1
            }
        };
        if e.kind == "tool_call" {
            rows[i].tools += 1;
        } else {
            rows[i].turns += 1;
            rows[i].ms += e.data["ms"].as_u64().unwrap_or(0);
            if e.data["tool_calls"].as_array().is_none_or(|a| a.is_empty()) {
                rows[i].prose += 1;
            }
        }
    }
    out.push_str("\nrole            model turns   model time   tool calls   prose-only turns\n");
    let (mut tt, mut tm) = (0, 0u64);
    for r in &rows {
        out.push_str(&format!(
            "{:<15} {:>11}   {:>10}   {:>10}   {:>16}\n",
            r.role,
            r.turns,
            fmt_ms(r.ms),
            r.tools,
            r.prose
        ));
        tt += r.turns;
        tm += r.ms;
    }
    out.push_str(&format!("{:<15} {:>11}   {:>10}\n", "total", tt, fmt_ms(tm)));

    let count = |kind: &str| events.iter().filter(|e| e.kind == kind).count();
    let failed_checks = events.iter().filter(|e| e.kind == "check" && e.data["passed"] == false).count();
    out.push_str(&format!(
        "\nchecks failed: {failed_checks} | forced structured outputs: {} | model errors: {}\n",
        count("terminal_forced"),
        count("model_error")
    ));
    let waits: Vec<u64> = events.iter().filter(|e| e.kind == "retry").map(|e| e.data["wait_s"].as_u64().unwrap_or(0)).collect();
    if !waits.is_empty() {
        out.push_str(&format!(
            "rate-limit and network waits: {} retries, {} total\n",
            waits.len(),
            fmt_ms(waits.iter().sum::<u64>() * 1000)
        ));
    }
    let paced: Vec<u64> = events.iter().filter(|e| e.kind == "paced").map(|e| e.data["wait_ms"].as_u64().unwrap_or(0)).collect();
    if !paced.is_empty() {
        out.push_str(&format!(
            "paced to stay under the per-minute limit: {} waits, {} total\n",
            paced.len(),
            fmt_ms(paced.iter().sum())
        ));
    }
    if let Some(e) = end {
        let d = &e.data;
        out.push_str(&format!(
            "result: {}/{} verified, {} proven, {} resolved earlier | calls: {} main + {} worker\n",
            d["verified"], d["confirmed"], d["proven"], d["resolved_earlier"], d["calls_main"], d["calls_worker"]
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("autoresolve-events-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ev(run: &str, role: &str, kind: &str, data: Value) -> Event {
        Event { seq: 1, ts_ms: 1000, run: run.into(), role: role.into(), kind: kind.into(), data }
    }

    #[tokio::test]
    async fn events_are_written_as_jsonl_and_tagged_with_the_acting_role() {
        let d = tmp("sink");
        let path = d.join("events.jsonl");
        init(&path, "r-test").unwrap();
        emit("unit_test_outside", json!({"a": 1}));
        scope("tester", async { emit("unit_test_inside", json!({"b": 2})) }).await;
        let evs = read_events(&path, Some("r-test")).unwrap();
        let outside = evs.iter().find(|e| e.kind == "unit_test_outside").unwrap();
        let inside = evs.iter().find(|e| e.kind == "unit_test_inside").unwrap();
        assert_eq!(outside.role, "controller");
        assert_eq!(inside.role, "tester");
        assert_eq!(inside.data["b"], 2);
        assert!(inside.seq > outside.seq);
    }

    #[test]
    fn reader_skips_partial_lines_and_filters_by_run() {
        let d = tmp("read");
        let path = d.join("events.jsonl");
        let a = serde_json::to_string(&ev("r1", "reviewer", "model_turn", json!({}))).unwrap();
        let b = serde_json::to_string(&ev("r2", "fixer", "model_turn", json!({}))).unwrap();
        std::fs::write(&path, format!("{a}\n{b}\n{{\"seq\": 3, \"ts_m")).unwrap(); // killed mid-write
        assert_eq!(read_events(&path, None).unwrap().len(), 2);
        let only = read_events(&path, Some("r2")).unwrap();
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].role, "fixer");
        assert_eq!(runs(&read_events(&path, None).unwrap()).len(), 2);
    }

    #[test]
    fn summary_counts_turns_time_tools_and_prose_per_role() {
        let turn = |role: &str, ms: u64, calls: Vec<&str>| {
            ev("r", role, "model_turn", json!({"ms": ms, "tool_calls": calls}))
        };
        let events = vec![
            ev("r", "controller", "run_start", json!({"command": "fix", "target": "a.py", "provider": "gemini"})),
            turn("reviewer", 2000, vec!["read_lines"]),
            ev("r", "reviewer", "tool_call", json!({"name": "read_lines"})),
            turn("reviewer", 3000, vec![]), // prose-only
            turn("fixer", 90_000, vec!["submit_patch"]),
            ev("r", "controller", "check", json!({"name": "x", "passed": false})),
            ev("r", "fixer", "terminal_forced", json!({})),
            ev("r", "reviewer", "paced", json!({"wait_ms": 4000})),
            ev("r", "reviewer", "retry", json!({"wait_s": 47, "reason": 429})),
            ev("r", "fixer", "retry", json!({"wait_s": 60, "reason": 429})),
            ev("r", "controller", "run_end", json!({"verified": 1, "confirmed": 2, "proven": 1, "resolved_earlier": 0, "calls_main": 3, "calls_worker": 1})),
        ];
        let text = summarize(&events);
        assert!(text.contains("reviewer"));
        let reviewer = text.lines().find(|l| l.starts_with("reviewer")).unwrap();
        assert!(reviewer.contains("5s")); // 2s + 3s
        let fixer = text.lines().find(|l| l.starts_with("fixer")).unwrap();
        assert!(fixer.contains("1m30s"));
        assert!(text.contains("checks failed: 1"));
        assert!(text.contains("forced structured outputs: 1"));
        assert!(text.contains("1/2 verified, 1 proven"));
        assert!(text.contains("2 retries, 1m47s total"));
        assert!(text.contains("1 waits, 4s total"));
        assert!(!text.contains("ollama=")); // not used in this run
    }

    #[test]
    fn truncation_is_character_safe() {
        assert_eq!(truncate("héllo wörld", 5), "héllo…");
        assert_eq!(truncate("short", 50), "short");
    }
}