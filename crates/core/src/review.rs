use crate::agent::{run_agent, Tools};
use crate::llm::{Provider, ToolCall, ToolSpec};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use futures_util::future::join_all;
use std::collections::{HashMap, HashSet};

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
    pub verdict: String, // "confirmed" | "refuted" | "uncertain"
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Judged {
    pub issue: Issue,
    pub verdict: Verdict,
}

const REVIEWER_SYSTEM: &str = "You are a senior code reviewer working inside a repository. \
Investigate with the tools: read the code, check callers and callees, and run static_findings \
(its results are hints, not proof). Report only real bugs and security problems that you \
verified by reading the code. Skip style nitpicks. When finished, call submit_findings \
exactly once, with an empty list if nothing is wrong.";

const SKEPTIC_SYSTEM: &str = "You are a skeptical senior engineer. A colleague claims to have \
found a bug. Your job is to try to REFUTE the claim, but only with evidence. Read the exact code, \
check callers and callees for guards or validation, and check whether the failing input is \
reachable. Rules: (1) A function with no callers in this repository is NOT unreachable: public \
functions are an API for outside code, so never refute a claim only because nothing here calls \
it. (2) Calls that appear only in test files say nothing about how production code uses a \
function. (3) Refute only when you can point to the specific lines that prevent the bug or show \
that the claimed behavior is wrong. Mark `confirmed` if you can point to the specific lines that \
make the bug real. Mark `refuted` only under rule 3. Mark `uncertain` if you cannot tell. Finish \
by calling submit_verdict.";

fn submit_findings_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_findings",
        description: "Submit the final list of verified issues. Call exactly once when done.",
        parameters: json!({
            "type": "object",
            "properties": {"findings": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "severity": {"type": "string", "enum": ["high", "medium", "low"]},
                    "file": {"type": "string"},
                    "line": {"type": "integer"},
                    "title": {"type": "string"},
                    "explanation": {"type": "string"},
                    "fix": {"type": "string"}
                },
                "required": ["severity", "file", "line", "title", "explanation", "fix"]
            }}},
            "required": ["findings"]
        }),
    }
}

pub fn submit_verdict_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_verdict",
        description: "Submit your verdict on the claimed bug. Call exactly once when done.",
        parameters: json!({
            "type": "object",
            "properties": {
                "verdict": {"type": "string", "enum": ["confirmed", "refuted", "uncertain"]},
                "reason": {"type": "string", "description": "Cite the specific lines or callers."}
            },
            "required": ["verdict", "reason"]
        }),
    }
}

/// The three specialist reviewers: (role name for the event log, what to focus on).
pub const FOCI: [(&str, &str); 3] = [
    (
        "reviewer_correctness",
        "CORRECTNESS: logic errors, wrong operators or comparisons, off-by-one and index errors, wrong arguments, \
         mishandled return values, state changed unexpectedly. Ignore security and style.",
    ),
    (
        "reviewer_security",
        "SECURITY: injection (shell, SQL, path), unsafe deserialization, hardcoded secrets, eval or exec on input, \
         missing validation of untrusted input. Report only real, exploitable problems. Ignore style.",
    ),
    (
        "reviewer_robustness",
        "ROBUSTNESS: edge cases (empty or None input, zero, negative numbers), unhandled exceptions, bare excepts \
         that hide errors, resource handling, mutable default arguments. Ignore security and style.",
    ),
];

pub async fn find_issues(provider: &dyn Provider, tools: &Tools<'_>, target: &str, max_steps: usize) -> Result<Vec<Issue>> {
    find_issues_focused(provider, tools, target, max_steps, None).await
}

async fn find_issues_focused(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    target: &str,
    max_steps: usize,
    focus: Option<(&'static str, &'static str)>,
) -> Result<Vec<Issue>> {
    let mut specs = Tools::specs();
    specs.push(submit_findings_spec());

    // Deterministic AST detectors run first; their findings become candidates for the model to verify.
    let hints = tools.call(&ToolCall { name: "static_findings".into(), args: json!({"file": target}) });
    let hint_text = hints
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| {
                    format!(
                        "- line {}: [{}] {}",
                        f["line"],
                        f["rule"].as_str().unwrap_or(""),
                        f["message"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    let mut task = format!(
        "Review `{target}` for real bugs and security problems. Start by reading the file with \
         read_lines (up to 200 lines per call), then call submit_findings."
    );
    if let Some((_, what)) = focus {
        task.push_str(&format!(
            "\n\nYour specialty is {what}\nOther reviewers cover the other areas, so report only problems in your own."
        ));
    }
    if !hint_text.is_empty() {
        if focus.is_none_or(|f| f.0 == FOCI[0].0) {
            eprintln!("[reviewer] static scanner found candidates:\n{hint_text}");
        }
        task.push_str(&format!(
            "\n\nA deterministic scanner already flagged these candidates:\n{hint_text}\n\
             Verify each against the code and include the real ones. Then look for problems the scanner \
             cannot see, such as wrong arguments, wrong indexes, unhandled edge cases and logic errors."
        ));
    }

    let role = focus.map(|f| f.0).unwrap_or("reviewer");
    let out = crate::events::scope(role, run_agent(provider, tools, REVIEWER_SYSTEM, &task, specs, "submit_findings", max_steps)).await?;
    if out["findings"].is_null() {
        eprintln!("[reviewer] submitted no `findings` field (treating as no issues): {out}");
        return Ok(vec![]); // models sometimes omit an empty list
    }
    serde_json::from_value(out["findings"].clone()).context("model returned malformed findings")
}

pub async fn challenge(provider: &dyn Provider, tools: &Tools<'_>, issue: &Issue, max_steps: usize) -> Result<Verdict> {
    let mut specs = Tools::specs();
    specs.push(submit_verdict_spec());
    let task = format!(
        "Claim to challenge:\n{}:{} [{}] {}\n{}\nProposed fix: {}",
        issue.file, issue.line, issue.severity, issue.title, issue.explanation, issue.fix
    );
    let out = crate::events::scope("skeptic", run_agent(provider, tools, SKEPTIC_SYSTEM, &task, specs, "submit_verdict", max_steps)).await?;
    serde_json::from_value(out).context("model returned a malformed verdict")
}

/// Reviewer proposes, Skeptic disposes.
pub async fn review(provider: &dyn Provider, tools: &Tools<'_>, target: &str, max_steps: usize) -> Result<Vec<Judged>> {
    let issues = find_issues(provider, tools, target, max_steps).await?;
    eprintln!("[reviewer] proposed {} issue(s); handing to the skeptic", issues.len());
    let mut out = Vec::new();
    for issue in issues {
        eprintln!("[skeptic] challenging: {}", issue.title);
        let verdict = match challenge(provider, tools, &issue, max_steps).await {
            Ok(v) => v,
            Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
            // one failed challenge should not sink the whole review
            Err(e) => Verdict { verdict: "uncertain".into(), reason: format!("skeptic failed: {e}") },
        };
        out.push(Judged { issue, verdict });
    }
    Ok(out)
}
/// How many reviewer runs to make and how to combine them.
/// `specialists` 0 = one generalist reviewer (the default); 1 to 3 = that many specialists, run concurrently.
/// `votes` = how many times each reviewer runs; with votes > 1 an issue must be found in `quorum` runs.
#[derive(Debug, Clone)]
pub struct ReviewOpts {
    pub specialists: usize,
    pub votes: usize,
    pub min_votes: Option<usize>,
}

impl Default for ReviewOpts {
    fn default() -> Self {
        Self { specialists: 0, votes: 1, min_votes: None }
    }
}

impl ReviewOpts {
    pub fn is_default(&self) -> bool {
        self.specialists == 0 && self.votes <= 1
    }
    /// Runs of one reviewer that must agree: 1 without voting, a majority with it.
    pub fn quorum(&self) -> usize {
        self.min_votes.unwrap_or(if self.votes <= 1 { 1 } else { self.votes / 2 + 1 }).max(1)
    }
}

fn severity_rank(s: &str) -> u8 {
    match s.to_ascii_lowercase().as_str() {
        "high" | "critical" => 3,
        "medium" => 2,
        _ => 1,
    }
}

/// Merge the findings of several reviewer runs. Findings in the same file within 2 lines are one issue.
/// An issue is kept when one reviewer found it in at least `quorum` of its runs, or when two different
/// specialists found it. Returns each kept issue (the most severe wording) with how many runs found it.
pub fn merge_findings(runs: &[(usize, Vec<Issue>)], quorum: usize) -> Vec<(Issue, usize)> {
    struct Cluster {
        file: String,
        line: u32,
        members: Vec<(usize, usize, Issue)>, // (run index, focus id, issue)
    }
    let norm = |f: &str| f.trim_start_matches("./").to_string();
    let mut clusters: Vec<Cluster> = Vec::new();
    for (run, (focus, issues)) in runs.iter().enumerate() {
        for issue in issues {
            let file = norm(&issue.file);
            match clusters.iter_mut().find(|c| c.file == file && c.line.abs_diff(issue.line) <= 2) {
                Some(c) => c.members.push((run, *focus, issue.clone())),
                None => clusters.push(Cluster { file, line: issue.line, members: vec![(run, *focus, issue.clone())] }),
            }
        }
    }
    let mut out = Vec::new();
    for c in clusters {
        let mut per_focus: HashMap<usize, HashSet<usize>> = HashMap::new();
        for (run, focus, _) in &c.members {
            per_focus.entry(*focus).or_default().insert(*run);
        }
        let best_votes = per_focus.values().map(|r| r.len()).max().unwrap_or(0);
        if best_votes < quorum && per_focus.len() < 2 {
            continue;
        }
        let agreeing: HashSet<usize> = c.members.iter().map(|m| m.0).collect();
        let best = c
            .members
            .iter()
            .max_by_key(|(_, _, i)| (severity_rank(&i.severity), i.explanation.len()))
            .map(|m| m.2.clone())
            .unwrap();
        out.push((best, agreeing.len()));
    }
    out.sort_by_key(|(i, _)| i.line);
    out
}

/// Reviewer ensemble: several reviewer runs at once, merged, then every skeptic challenge at once.
pub async fn review_with(
    provider: &dyn Provider,
    tools: &Tools<'_>,
    target: &str,
    max_steps: usize,
    opts: &ReviewOpts,
) -> Result<Vec<Judged>> {
    if opts.is_default() {
        return review(provider, tools, target, max_steps).await;
    }
    let foci: Vec<Option<(&'static str, &'static str)>> = if opts.specialists == 0 {
        vec![None]
    } else {
        FOCI[..opts.specialists.min(FOCI.len())].iter().map(|f| Some(*f)).collect()
    };
    let votes = opts.votes.max(1);
    let mut jobs: Vec<(usize, Option<(&'static str, &'static str)>)> = Vec::new();
    for (fi, f) in foci.iter().enumerate() {
        for _ in 0..votes {
            jobs.push((fi, *f));
        }
    }
    eprintln!("[reviewer] running {} reviewer run(s) at once ({} specialist(s) x {} vote(s))", jobs.len(), foci.len(), votes);
    let results = join_all(jobs.iter().map(|(_, f)| find_issues_focused(provider, tools, target, max_steps, *f))).await;

    let mut runs: Vec<(usize, Vec<Issue>)> = Vec::new();
    let mut first_err = None;
    for ((fi, _), r) in jobs.iter().zip(results) {
        match r {
            Ok(v) => runs.push((*fi, v)),
            Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
            Err(e) => {
                eprintln!("[reviewer] one run failed and is ignored: {e}");
                first_err.get_or_insert(e);
            }
        }
    }
    if runs.is_empty() {
        return Err(first_err.unwrap_or_else(|| anyhow::anyhow!("no reviewer run completed")));
    }
    let found: Vec<usize> = runs.iter().map(|r| r.1.len()).collect();
    let merged = merge_findings(&runs, opts.quorum());
    crate::events::emit(
        "ensemble",
        json!({"runs": runs.len(), "found_per_run": found, "kept": merged.len(), "quorum": opts.quorum(),
               "agreement": merged.iter().map(|m| m.1).collect::<Vec<_>>()}),
    );
    eprintln!("[reviewer] runs found {:?} issue(s); {} kept after merging; handing to the skeptics", found, merged.len());

    let verdicts = join_all(merged.iter().map(|(i, _)| challenge(provider, tools, i, max_steps))).await;
    let mut out = Vec::new();
    for ((issue, _), v) in merged.into_iter().zip(verdicts) {
        let verdict = match v {
            Ok(v) => v,
            Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
            Err(e) => Verdict { verdict: "uncertain".into(), reason: format!("skeptic failed: {e}") },
        };
        out.push(Judged { issue, verdict });
    }
    Ok(out)
}

const STILL_PRESENT_SYSTEM: &str = "You decide whether a reported bug STILL EXISTS in the current \
code. Other fixes may already have been applied, so the code and its line numbers may have changed \
since the report. Read the relevant code with the tools. Verdict `confirmed` means the described \
problem is still present. Verdict `refuted` means it no longer exists in the current code (cite the \
lines that show it). Verdict `uncertain` if you cannot tell. Judge only whether the problem is \
present now, not whether the original report was reasonable. Finish by calling submit_verdict.";

/// After other fixes have landed: is this issue still present in the staged code?
pub async fn still_present(provider: &dyn Provider, tools: &Tools<'_>, issue: &Issue, max_steps: usize) -> Result<Verdict> {
    let mut specs = Tools::specs();
    specs.push(submit_verdict_spec());
    let task = format!(
        "Reported bug:\n{}:{} [{}] {}\n{}\n\nIs this problem still present in the current code?",
        issue.file, issue.line, issue.severity, issue.title, issue.explanation
    );
    let out = crate::events::scope("still_present", run_agent(provider, tools, STILL_PRESENT_SYSTEM, &task, specs, "submit_verdict", max_steps)).await?;
    serde_json::from_value(out).context("model returned a malformed verdict")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use crate::llm::{Message, ModelTurn};
    use async_trait::async_trait;
    use std::path::Path;
    use std::sync::Mutex;

    fn issue(file: &str, line: u32, sev: &str, title: &str) -> Issue {
        Issue {
            severity: sev.into(),
            file: file.into(),
            line,
            title: title.into(),
            explanation: format!("{title} explained"),
            fix: "fix".into(),
        }
    }

    #[test]
    fn nearby_findings_merge_and_the_most_severe_wording_wins() {
        let runs = vec![
            (0, vec![issue("./a.py", 10, "low", "x")]),
            (1, vec![issue("a.py", 11, "high", "y")]), // same place, another specialist
            (2, vec![issue("a.py", 40, "medium", "z")]), // only one specialist saw this
        ];
        let merged = merge_findings(&runs, 1);
        assert_eq!(merged.len(), 2);
        // the merged issue is the most severe member, with that member's own line
        assert_eq!((merged[0].0.line, merged[0].0.severity.as_str(), merged[0].1), (11, "high", 2));
        assert_eq!(merged[1].0.line, 40);
        assert_eq!(merged[1].1, 1);
    }

    #[test]
    fn voting_drops_findings_that_too_few_runs_agree_on() {
        // one reviewer, three runs, quorum 2: A is seen three times, B once
        let runs = vec![
            (0, vec![issue("a.py", 5, "high", "A")]),
            (0, vec![issue("a.py", 5, "high", "A"), issue("a.py", 30, "medium", "B")]),
            (0, vec![issue("a.py", 6, "high", "A")]),
        ];
        let merged = merge_findings(&runs, 2);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].1, 3);
        // the same finding twice inside ONE run counts once
        let dup = vec![(0, vec![issue("a.py", 5, "low", "A"), issue("a.py", 6, "low", "A")])];
        assert!(merge_findings(&dup, 2).is_empty());
        // a different reviewer agreeing saves a finding that voting alone would drop
        let two = vec![(0, vec![issue("a.py", 5, "low", "A")]), (1, vec![issue("a.py", 5, "low", "A")])];
        assert_eq!(merge_findings(&two, 2).len(), 1);
    }

    #[test]
    fn quorum_defaults_to_a_majority_only_when_voting() {
        assert_eq!(ReviewOpts::default().quorum(), 1);
        assert_eq!(ReviewOpts { specialists: 3, votes: 1, min_votes: None }.quorum(), 1);
        assert_eq!(ReviewOpts { specialists: 0, votes: 3, min_votes: None }.quorum(), 2);
        assert_eq!(ReviewOpts { specialists: 0, votes: 5, min_votes: Some(4) }.quorum(), 4);
        assert!(ReviewOpts::default().is_default());
    }

    /// Fake model: each reviewer run reports what its specialty (or its turn in line) is scripted to report.
    struct Scripted {
        submit_count: Mutex<usize>,
        generalist_runs: Vec<Vec<serde_json::Value>>,
    }

    #[async_trait]
    impl Provider for Scripted {
        async fn complete(&self, _s: &str, history: &[Message], tools: &[ToolSpec]) -> Result<ModelTurn> {
            let looked = history.iter().any(|m| matches!(m, Message::ToolResults(_)));
            let task = match history.first() {
                Some(Message::User(t)) => t.clone(),
                _ => String::new(),
            };
            let call = if !looked {
                ToolCall { name: "list_symbols".into(), args: json!({}) }
            } else {
                let terminal = tools.iter().map(|t| t.name).find(|n| n.starts_with("submit_")).unwrap();
                let args = match terminal {
                    "submit_findings" => {
                        let f = |line: u32, title: &str, sev: &str| {
                            json!({"severity": sev, "file": "m.py", "line": line, "title": title, "explanation": "e", "fix": "f"})
                        };
                        if task.contains("Your specialty is SECURITY") {
                            json!({"findings": [f(9, "injection", "high")]})
                        } else if task.contains("Your specialty is CORRECTNESS") {
                            json!({"findings": [f(5, "wrong operator", "medium")]})
                        } else if task.contains("Your specialty is ROBUSTNESS") {
                            json!({"findings": [f(6, "wrong operator edge", "low")]}) // same spot as correctness
                        } else {
                            let mut n = self.submit_count.lock().unwrap();
                            let run = self.generalist_runs[*n % self.generalist_runs.len()].clone();
                            *n += 1;
                            json!({"findings": run})
                        }
                    }
                    "submit_verdict" => json!({"verdict": "confirmed", "reason": "r"}),
                    other => panic!("unexpected terminal {other}"),
                };
                ToolCall { name: terminal.into(), args }
            };
            Ok(ModelTurn { text: String::new(), calls: vec![call], raw: json!({}) })
        }
    }

    fn setup(name: &str) -> (std::path::PathBuf, Graph) {
        let d = std::env::temp_dir().join(format!("autoresolve-review-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("m.py"), "x = 1\n").unwrap();
        (d, Graph::open(Path::new(":memory:")).unwrap())
    }

    #[tokio::test]
    async fn three_specialists_run_together_and_their_findings_are_merged() {
        let (d, graph) = setup("spec");
        let tools = Tools::new(&graph, &d).unwrap();
        let model = Scripted { submit_count: Mutex::new(0), generalist_runs: vec![vec![]] };
        let opts = ReviewOpts { specialists: 3, votes: 1, min_votes: None };
        let judged = review_with(&model, &tools, "m.py", 5, &opts).await.unwrap();
        // correctness (line 5) and robustness (line 6) are the same finding; security found another
        let lines: Vec<u32> = judged.iter().map(|j| j.issue.line).collect();
        assert_eq!(lines, vec![5, 9]);
        assert!(judged.iter().all(|j| j.verdict.verdict == "confirmed"));
    }

    #[tokio::test]
    async fn voting_keeps_what_most_runs_found_and_drops_one_off_findings() {
        let (d, graph) = setup("vote");
        let tools = Tools::new(&graph, &d).unwrap();
        let f = |line: u32, title: &str| json!({"severity": "high", "file": "m.py", "line": line, "title": title, "explanation": "e", "fix": "f"});
        let model = Scripted {
            submit_count: Mutex::new(0),
            generalist_runs: vec![vec![f(5, "A")], vec![f(5, "A"), f(20, "B")], vec![f(5, "A")]],
        };
        let opts = ReviewOpts { specialists: 0, votes: 3, min_votes: None };
        let judged = review_with(&model, &tools, "m.py", 5, &opts).await.unwrap();
        assert_eq!(judged.len(), 1);
        assert_eq!(judged[0].issue.line, 5);
    }
}