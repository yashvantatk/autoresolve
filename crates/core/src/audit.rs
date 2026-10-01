use crate::agent::Tools;
use crate::llm::{Message, Provider};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const REVIEWER_SYSTEM: &str = "You are a code reviewer working inside a repository. \
Investigate with the tools before concluding: list symbols, read the code, check callers and \
callees, and run static_findings (treat it as hints, not proof). Only include issues you verified \
by reading the code. Your FINAL reply must be ONLY a JSON array, with no prose and no code fences: \
[{\"severity\":\"high|medium|low\",\"file\":\"path\",\"line\":123,\"title\":\"short title\",\
\"explanation\":\"what is wrong and why it matters\",\"fix\":\"concrete fix\"}]. \
Return [] if there are no issues.";

const SKEPTIC_SYSTEM: &str = "You are a skeptical senior engineer. You are given an issue claimed by \
another reviewer. Your job is to try to REFUTE it. Read the code at the cited location, check callers \
and callees, and consider whether the bad state is actually reachable, whether the behaviour is \
intended or already guarded, and whether the claim misreads the code. Your FINAL reply must be ONLY a \
JSON object, with no prose and no code fences: {\"verdict\":\"confirmed|refuted|uncertain\",\
\"reason\":\"one or two sentences citing file:line evidence\"}. Say confirmed only if you tried to \
refute the claim and could not.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub severity: String,
    pub file: String,
    pub line: u32,
    pub title: String,
    pub explanation: String,
    pub fix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub verdict: String, // confirmed | refuted | uncertain
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct Audited {
    pub issue: Issue,
    pub verdict: Verdict,
}

/// One tool-using agent run: ask the model, execute its tool calls, repeat until it answers.
async fn run_agent(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    system: &str,
    task: &str,
    max_steps: usize,
    label: &str,
) -> Result<String> {
    let specs = Tools::specs();
    let mut history = vec![Message::User(task.to_string())];
    for step in 1..=max_steps {
        let turn = provider.complete(system, &history, &specs).await?;
        let calls = turn.calls.clone();
        let text = turn.text.clone();
        history.push(Message::Model(turn));
        if calls.is_empty() {
            return Ok(text);
        }
        let mut results = Vec::new();
        for c in &calls {
            eprintln!("[{label} step {step}] {}({})", c.name, c.args);
            results.push((c.name.clone(), tools.call(c)));
        }
        history.push(Message::ToolResults(results));
    }
    bail!("{label} hit the step limit ({max_steps}) without a final answer")
}

/// Pull the JSON out of a reply that may be wrapped in prose or code fences.
fn extract_json(text: &str, open: char, close: char) -> Result<&str> {
    let s = text.find(open).context("no JSON found in reply")?;
    let e = text.rfind(close).context("no JSON found in reply")?;
    if e < s {
        bail!("malformed JSON in reply");
    }
    Ok(&text[s..=e])
}

pub async fn find_issues(provider: &dyn Provider, tools: &Tools<'_>, target: &str, max_steps: usize) -> Result<Vec<Issue>> {
    let task = format!("Review `{target}` for bugs, security problems and anti-patterns.");
    let reply = run_agent(provider, tools, REVIEWER_SYSTEM, &task, max_steps, "reviewer").await?;
    let json = extract_json(&reply, '[', ']')?;
    serde_json::from_str(json).with_context(|| format!("reviewer reply was not a valid issue list:\n{reply}"))
}

pub async fn challenge(provider: &dyn Provider, tools: &Tools<'_>, issue: &Issue, max_steps: usize) -> Result<Verdict> {
    let task = format!(
        "Claimed issue:\n{}\n\nTry to refute it.",
        serde_json::to_string_pretty(issue)?
    );
    let reply = run_agent(provider, tools, SKEPTIC_SYSTEM, &task, max_steps, "skeptic").await?;
    let json = extract_json(&reply, '{', '}')?;
    let mut v: Verdict = serde_json::from_str(json)
        .with_context(|| format!("skeptic reply was not a valid verdict:\n{reply}"))?;
    v.verdict = v.verdict.trim().to_lowercase();
    if !["confirmed", "refuted", "uncertain"].contains(&v.verdict.as_str()) {
        v.verdict = "uncertain".into();
    }
    Ok(v)
}

pub async fn audit(provider: &dyn Provider, tools: &Tools<'_>, target: &str, max_steps: usize) -> Result<Vec<Audited>> {
    let issues = find_issues(provider, tools, target, max_steps).await?;
    eprintln!("[audit] reviewer proposed {} issue(s); cross-examining each...", issues.len());
    let mut out = Vec::new();
    for issue in issues {
        let verdict = match challenge(provider, tools, &issue, max_steps).await {
            Ok(v) => v,
            // a failed skeptic must never silently promote or drop a finding
            Err(e) => Verdict { verdict: "uncertain".into(), reason: format!("skeptic failed: {e}") },
        };
        eprintln!("[audit] \"{}\" -> {}", issue.title, verdict.verdict);
        out.push(Audited { issue, verdict });
    }
    Ok(out)
}

pub fn render(audited: &[Audited]) -> String {
    let mut out = String::new();
    for (label, heading) in [("confirmed", "Confirmed"), ("uncertain", "Needs human review")] {
        let group: Vec<&Audited> = audited.iter().filter(|a| a.verdict.verdict == label).collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("## {heading} ({})\n\n", group.len()));
        for a in group {
            out.push_str(&format!(
                "### [{}] {} ({}:{})\n{}\n\n**Fix:** {}\n\n*Skeptic:* {}\n\n",
                a.issue.severity.to_uppercase(),
                a.issue.title,
                a.issue.file,
                a.issue.line,
                a.issue.explanation,
                a.issue.fix,
                a.verdict.reason
            ));
        }
    }
    let refuted: Vec<&Audited> = audited.iter().filter(|a| a.verdict.verdict == "refuted").collect();
    if !refuted.is_empty() {
        out.push_str(&format!("## Discarded by skeptic ({})\n", refuted.len()));
        for a in refuted {
            out.push_str(&format!("- {} ({}:{}): {}\n", a.issue.title, a.issue.file, a.issue.line, a.verdict.reason));
        }
    }
    if out.is_empty() {
        out.push_str("No issues found.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::llm::{ModelTurn, ToolSpec};
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Mutex;

    /// Fake model that replays scripted replies, so the pipeline is testable with no API key.
    struct Script(Mutex<Vec<String>>);

    #[async_trait]
    impl Provider for Script {
        async fn complete(&self, _s: &str, _h: &[Message], _t: &[ToolSpec]) -> Result<ModelTurn> {
            let text = self.0.lock().unwrap().remove(0);
            Ok(ModelTurn { text, calls: vec![], raw: json!({}) })
        }
    }

    #[tokio::test]
    async fn skeptic_filters_findings() {
        let issues = r#"```json
[{"severity":"high","file":"a.py","line":3,"title":"real bug","explanation":"e1","fix":"f1"},
 {"severity":"low","file":"a.py","line":9,"title":"false alarm","explanation":"e2","fix":"f2"}]
```"#;
        let provider = Script(Mutex::new(vec![
            issues.to_string(),
            r#"{"verdict":"Confirmed","reason":"checked a.py:3"}"#.to_string(),
            r#"{"verdict":"refuted","reason":"guarded on line 8"}"#.to_string(),
        ]));
        let graph = Graph::open(Path::new(":memory:")).unwrap();
        let tools = Tools::new(&graph, Path::new(".")).unwrap();

        let audited = audit(&provider, &tools, "a.py", 3).await.unwrap();
        assert_eq!(audited.len(), 2);
        let report = render(&audited);
        assert!(report.contains("## Confirmed (1)"));
        assert!(report.contains("## Discarded by skeptic (1)"));
    }

    #[test]
    fn extracts_json_from_noisy_reply() {
        let r = "Here you go:\n```json\n{\"a\": 1}\n```\nthanks";
        assert_eq!(extract_json(r, '{', '}').unwrap(), "{\"a\": 1}");
    }
}
```