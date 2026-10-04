use anyhow::{Context, Result};
use autoresolve_core::agent::Tools;
use autoresolve_core::events;
use autoresolve_core::fix::{self, Plan, PlanItem};
use autoresolve_core::graph::{self, FileGraph, Graph};
use autoresolve_core::llm::{self, Provider};
use autoresolve_core::policy::Policy;
use autoresolve_core::report;
use autoresolve_core::review;
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::{Path, PathBuf};

mod tui;

#[derive(Parser)]
#[command(name = "autoresolve", version, about = "Agentic code review & repair")]
struct Cli {
    /// Where the repo graph database lives
    #[arg(long, global = true, default_value = ".autoresolve/graph.db")]
    db: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

/// Report formats for `scan` and `review`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Format {
    Text,
    Json,
    Markdown,
    Sarif,
}

/// Reviewer ensemble settings: flags win, then the AUTORESOLVE_* variables, then the single-reviewer default.
fn review_opts(reviewers: Option<usize>, votes: Option<usize>, min_votes: Option<usize>) -> review::ReviewOpts {
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<usize>().ok());
    review::ReviewOpts {
        specialists: reviewers.or_else(|| env("AUTORESOLVE_REVIEWERS")).unwrap_or(0).min(3),
        votes: votes.or_else(|| env("AUTORESOLVE_VOTES")).unwrap_or(1).max(1),
        min_votes: min_votes.or_else(|| env("AUTORESOLVE_MIN_VOTES")),
    }
}

/// Print a report, or write it to a file when --out is given.
fn emit(out: &Option<PathBuf>, text: &str) -> Result<()> {
    match out {
        Some(p) => {
            std::fs::write(p, text)?;
            eprintln!("[report] wrote {}", p.display());
        }
        None => println!("{text}"),
    }
    Ok(())
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the syntax tree of a Python file
    Ast { file: PathBuf },
    /// Scan a directory for anti-patterns
    Scan {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit findings as JSON (for CI and tooling); same as --format json
        #[arg(long)]
        json: bool,
        /// Output format: text, json or sarif
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Write the report to this file instead of printing it
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Index functions, classes and calls into the repo graph
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Who calls this function?
    Callers { name: String },
    /// What does this function call?
    Callees { name: String },
    /// List every indexed symbol
    Symbols,
    /// Agentic code review of a file (Gemini needs GEMINI_API_KEY; or AUTORESOLVE_PROVIDER=ollama)
    Review {
        file: PathBuf,
        /// Repository root the agent may read from
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Maximum agent steps before giving up
        #[arg(long, default_value_t = 12)]
        max_steps: usize,
        /// Specialist reviewers to run at once (0 = one generalist; up to 3: correctness, security, robustness).
        /// Also read from AUTORESOLVE_REVIEWERS.
        #[arg(long)]
        reviewers: Option<usize>,
        /// Run each reviewer this many times and keep issues that enough runs agree on (default 1).
        /// Also read from AUTORESOLVE_VOTES.
        #[arg(long)]
        votes: Option<usize>,
        /// Runs that must agree when voting (default: a majority). Also AUTORESOLVE_MIN_VOTES.
        #[arg(long)]
        min_votes: Option<usize>,
        /// Output format: text, json, markdown or sarif
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Write the report to this file instead of printing it
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Review a file, then generate and verify a fix for each confirmed issue (saves a plan)
    Fix {
        file: PathBuf,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Command to run in the sandbox after patching, e.g. "pytest -q"
        #[arg(long)]
        test_cmd: Option<String>,
        /// Write verified patches into the real repo while running (default is a dry run)
        #[arg(long)]
        apply: bool,
        #[arg(long, default_value_t = 12)]
        max_steps: usize,
        /// Specialist reviewers to run at once (0 = one generalist; up to 3: correctness, security, robustness).
        /// Also read from AUTORESOLVE_REVIEWERS.
        #[arg(long)]
        reviewers: Option<usize>,
        /// Run each reviewer this many times and keep issues that enough runs agree on (default 1).
        /// Also read from AUTORESOLVE_VOTES.
        #[arg(long)]
        votes: Option<usize>,
        /// Runs that must agree when voting (default: a majority). Also AUTORESOLVE_MIN_VOTES.
        #[arg(long)]
        min_votes: Option<usize>,
        /// Fix a bug you describe instead of running the reviewer, e.g. --issue "parse_int swallows TypeError"
        #[arg(long)]
        issue: Option<String>,
    },
    /// Summarize a recorded run: per-role model calls, time, tool use and outcome (no models involved)
    Events {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Run id to show (default: the latest run)
        #[arg(long)]
        run: Option<String>,
        /// List all recorded runs
        #[arg(long)]
        list: bool,
        /// Print the raw JSON events of the run
        #[arg(long)]
        raw: bool,
    },
    /// Apply the plan saved by the last `fix` run, exactly as reviewed (no models involved)
    ApplyPlan {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Plan file (default: <root>/.autoresolve/plan.json)
        #[arg(long)]
        plan: Option<PathBuf>,
        /// Also apply fixes that no regression test proves (read their diffs first)
        #[arg(long)]
        include_unproven: bool,
    },
    /// Terminal UI: watch a run live, replay it, read the diffs (reads .autoresolve/events.jsonl, no models)
    Tui {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Run id to show (default: the newest run in the log)
        #[arg(long)]
        run: Option<String>,
    },
        /// Run a command in the sandbox (Docker: no network, read-only mount unless --writable)
    Sandbox {
        cmd: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        /// Mount the directory writable (default is read-only)
        #[arg(long)]
        writable: bool,
    },
}

/// Turn a free-text bug report into an Issue. If the text names a function defined in the file,
/// the issue points into that function so its docstring (the contract) reaches the tester, fixer and gate.
fn issue_from_report(root: &Path, file: &Path, text: &str) -> review::Issue {
    let src = std::fs::read_to_string(root.join(file)).unwrap_or_default();
    let words: Vec<&str> = text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    let mut line = 1usize;
    for (n, l) in src.lines().enumerate() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("def ").or_else(|| t.strip_prefix("async def ")) {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() && words.contains(&name.as_str()) {
                line = n + 2; // a line inside the function body
                break;
            }
        }
    }
    let title: String = text.chars().take(80).collect();
    review::Issue {
        severity: "medium".into(),
        file: file.display().to_string(),
        line: line as u32,
        title,
        explanation: text.to_string(),
        fix: "none given: decide from the code and its documented contract".into(),
    }
}

fn python_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root).build() {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("py") {
            out.push(p.to_path_buf());
        }
    }
    Ok(out)
}

fn index_repo(path: &Path, db: &Path) -> Result<(usize, usize, usize)> {
    let mut files: Vec<(String, FileGraph)> = Vec::new();
    for p in python_files(path)? {
        let Ok(src) = std::fs::read_to_string(&p) else { continue };
        let tree = autoresolve_core::parse_python(&src)?;
        files.push((p.display().to_string(), graph::extract(&src, &tree)));
    }
    let mut g = Graph::open(db)?;
    g.replace_all(&files)?;
    let syms: usize = files.iter().map(|(_, f)| f.symbols.len()).sum();
    let calls: usize = files.iter().map(|(_, f)| f.calls.len()).sum();
    Ok((files.len(), syms, calls))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Ast { file } => {
            let src = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let tree = autoresolve_core::parse_python(&src)?;
            let mut out = String::new();
            autoresolve_core::dump(tree.root_node(), &src, 0, &mut out);
            print!("{out}");
        }
        Cmd::Scan { path, json, format, out } => {
            let mut findings = Vec::new();
            for p in python_files(&path)? {
                let Ok(src) = std::fs::read_to_string(&p) else { continue };
                findings.extend(autoresolve_core::detectors::scan_python(&p, &src)?);
            }
            let format = if json { Format::Json } else { format };
            if format == Format::Markdown {
                anyhow::bail!("scan supports text, json and sarif (markdown is for review)");
            }
            if format == Format::Json {
                emit(&out, &serde_json::to_string_pretty(&findings)?)?;
            } else if format == Format::Sarif {
                emit(&out, &serde_json::to_string_pretty(&report::sarif_from_scan(&findings))?)?;
            } else {
                for f in &findings {
                    println!("{}:{}:{}  [{}] {}", f.file.display(), f.line, f.col, f.rule, f.message);
                }
                println!("\n{} finding(s)", findings.len());
            }
            if !findings.is_empty() {
                std::process::exit(1);
            }
        }
        Cmd::Index { path } => {
            let (n, syms, calls) = index_repo(&path, &cli.db)?;
            println!("indexed {n} files, {syms} symbols, {calls} calls -> {}", cli.db.display());
        }
        Cmd::Callers { name } => {
            let g = Graph::open(&cli.db)?;
            let rows = g.callers(&name)?;
            for (caller, file, line) in &rows {
                println!("{caller}  {file}:{line}");
            }
            println!("\n{} caller(s) of `{name}`", rows.len());
        }
        Cmd::Callees { name } => {
            let g = Graph::open(&cli.db)?;
            let rows = g.callees(&name)?;
            for callee in &rows {
                println!("{callee}");
            }
            println!("\n`{name}` calls {} function(s)", rows.len());
        }
        Cmd::Symbols => {
            let g = Graph::open(&cli.db)?;
            for (file, kind, qualname, start, end) in g.symbols()? {
                println!("{file}:{start}-{end}  {kind:<8} {qualname}");
            }
        }
        Cmd::Review { file, root, max_steps, reviewers, votes, min_votes, format, out } => {
            let provider = llm::provider_from_env(false)?;
            events::init(&root.canonicalize()?.join(".autoresolve").join(events::LOG_FILE), &events::new_run_id())?;
            events::emit("run_start", events::run_config("review", &file.display().to_string()));
            index_repo(&root, &cli.db)?; // always review against a fresh graph
            let graph = Graph::open(&cli.db)?;
            let tools = Tools::new(&graph, &root)?;
            let opts = review_opts(reviewers, votes, min_votes);
            events::emit("review_config", serde_json::json!({"reviewers": opts.specialists, "votes": opts.votes, "quorum": opts.quorum()}));
            let judged = review::review_with(&provider, &tools, &file.display().to_string(), max_steps, &opts).await?;
            events::emit(
                "run_end",
                serde_json::json!({
                    "verified": 0, "confirmed": judged.iter().filter(|j| j.verdict.verdict == "confirmed").count(),
                    "proven": 0, "resolved_earlier": 0, "calls_main": provider.calls(), "calls_worker": 0
                }),
            );
            match format {
                Format::Json => emit(&out, &serde_json::to_string_pretty(&judged)?)?,
                Format::Markdown => emit(&out, &report::markdown_from_review(&judged))?,
                Format::Sarif => emit(&out, &serde_json::to_string_pretty(&report::sarif_from_review(&judged))?)?,
                Format::Text => {}
            }
            for (label, want) in [("CONFIRMED", "confirmed"), ("UNCERTAIN", "uncertain"), ("REFUTED by skeptic", "refuted")] {
                if format != Format::Text {
                    break;
                }
                let group: Vec<_> = judged.iter().filter(|j| j.verdict.verdict == want).collect();
                if group.is_empty() {
                    continue;
                }
                println!("\n== {label} ({}) ==", group.len());
                for j in group {
                    let i = &j.issue;
                    println!(
                        "[{}] {}:{}  {}\n  {}\n  fix: {}\n  skeptic: {}",
                        i.severity.to_uppercase(), i.file, i.line, i.title, i.explanation, i.fix, j.verdict.reason
                    );
                }
            }
            eprintln!("[usage] {} model calls", provider.calls());
        }
        Cmd::Fix { file, root, test_cmd, apply, max_steps, reviewers, votes, min_votes, issue } => {
            let provider = llm::provider_from_env(false)?; // reviewer and skeptic
            let strong = llm::provider_from_env(true)?; // tester, fixer and patch gate
            index_repo(&root, &cli.db)?;
            eprintln!("[sandbox] {}", autoresolve_core::sandbox::describe());
            let graph = Graph::open(&cli.db)?;
            let real = root.canonicalize()?;
            let started = std::time::Instant::now();
            events::init(&real.join(".autoresolve").join(events::LOG_FILE), &events::new_run_id())?;
            events::emit("run_start", events::run_config("fix", &file.display().to_string()));
            let policy = Policy::load(&real)?; // a broken policy.toml stops the run here
            eprintln!("[policy] {}", policy.describe());
            fix::clean_scratch(&real); // start from a clean slate: no stale copies for the models to wander into
            // Verified fixes accumulate in a staging copy, so later fixes are tested on top of earlier ones.
            let work = fix::create_sandbox(&real, "work")?;
            let tools = Tools::new(&graph, &work)?;
            let target = file.display().to_string();
            let opts = review_opts(reviewers, votes, min_votes);
            events::emit("review_config", serde_json::json!({"reviewers": opts.specialists, "votes": opts.votes, "quorum": opts.quorum()}));
            let judged = match &issue {
                // issue-driven mode: the user's bug report replaces the reviewer and the skeptic
                Some(text) => vec![review::Judged {
                    issue: issue_from_report(&work, &file, text),
                    verdict: review::Verdict {
                        verdict: "confirmed".into(),
                        reason: "reported by the user (--issue); reviewer and skeptic skipped".into(),
                    },
                }],
                None => review::review_with(&provider, &tools, &target, max_steps, &opts).await?,
            };
            let confirmed: Vec<_> = judged.into_iter().filter(|j| j.verdict.verdict == "confirmed").collect();
            events::emit("review_done", serde_json::json!({"confirmed": confirmed.len()}));
            println!("\n{} confirmed issue(s) to fix", confirmed.len());

            let mut plan = Plan::default();
            let (mut verified, mut proven, mut already) = (0, 0, 0);
            let mut queue: Vec<(usize, &review::Judged)> = confirmed.iter().enumerate().collect();
            for round in 1..=2 {
                let mut deferred = Vec::new();
                for (n, j) in queue {
                    let i = &j.issue;
                    println!("\n=== [{}] {}:{}  {} ===", i.severity.to_uppercase(), i.file, i.line, i.title);
                    events::emit(
                        "issue_start",
                        serde_json::json!({"n": n, "round": round, "title": i.title, "file": i.file, "line": i.line, "severity": i.severity}),
                    );

                    // an earlier fix may already have resolved this one: check before spending calls on it
                    // (only proven fixes are stacked, so only they can have changed the code)
                    if proven > 0 {
                        match review::still_present(&provider, &tools, i, max_steps).await {
                            Ok(v) if v.verdict == "refuted" => {
                                println!("no longer present after the earlier fixes; skipping ({})", v.reason);
                                events::emit("skipped_resolved", serde_json::json!({"title": i.title}));
                                already += 1;
                                continue;
                            }
                            Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
                            _ => {}
                        }
                    }

                    let slug = fix::slugify(&i.title, n);
                    let siblings: Vec<review::Issue> = confirmed
                        .iter()
                        .enumerate()
                        .filter(|(m, _)| *m != n)
                        .map(|(_, x)| x.issue.clone())
                        .collect();

                    // reproduction first: a test that must FAIL on the current (stacked) code
                    let repro = match fix::reproduce(&strong, &tools, i, &slug, max_steps).await {
                        Ok(t) => {
                            println!("regression test: {} (fails on the current code, as it should)", t.description);
                            events::emit("repro", serde_json::json!({"ok": true, "description": t.description}));
                            Some(t)
                        }
                        Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
                        // Not evidence that the bug is gone: only the still-present check above may skip an
                        // issue as resolved. A passing test here more likely means the test is wrong.
                        Err(e) if e.to_string().contains("looks already fixed") => {
                            println!("the tests written for this bug pass on the current code, so it was not reproduced; the fix will be unproven");
                            events::emit("repro", serde_json::json!({"ok": false, "error": "test passed on the current code"}));
                            None
                        }
                        Err(e) => {
                            println!("could not reproduce the bug with a test ({e}); the fix will be unproven");
                            events::emit("repro", serde_json::json!({"ok": false, "error": events::truncate(&e.to_string(), 300)}));
                            None
                        }
                    };

                    // behavior guard: a test of ORDINARY use that passes now and must still pass after the patch
                    let guard = match fix::write_guard(&strong, &tools, i, &slug, max_steps).await {
                        Ok(g) => {
                            println!("behavior guard: {} (passes on the current code)", g.description);
                            events::emit("guard", serde_json::json!({"ok": true, "description": g.description}));
                            Some(g)
                        }
                        Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
                        Err(e) => {
                            println!("no behavior guard ({e}); the patch will not be checked against ordinary use");
                            events::emit("guard", serde_json::json!({"ok": false, "error": events::truncate(&e.to_string(), 300)}));
                            None
                        }
                    };

                    // `strong` (the cheap worker) writes the patch; `provider` (the strong model) judges it
                    let attempt = fix::fix_issue(
                        &strong,
                        &provider,
                        &tools,
                        i,
                        &siblings,
                        test_cmd.as_deref(),
                        repro.as_ref().map(|t| (slug.as_str(), t)),
                        guard.as_ref().map(|g| g.code.as_str()),
                        &policy,
                        &format!("fix{n}r{round}"),
                        max_steps,
                    )
                    .await;
                    match attempt {
                        Ok(o) => {
                            println!("{}", o.patch.summary);
                            print!("{}", o.diff);
                            for c in &o.checks {
                                println!("  [{}] {} {}", if c.passed { "PASS" } else { "FAIL" }, c.name, c.detail);
                                events::emit(
                                    "check",
                                    serde_json::json!({"name": c.name, "passed": c.passed, "detail": events::truncate(&c.detail, 400)}),
                                );
                            }
                            events::emit(
                                "outcome",
                                serde_json::json!({"title": i.title, "verified": o.verified, "proven": o.proven, "summary": o.patch.summary}),
                            );
                            if o.verified {
                                verified += 1;
                                plan.items.push(PlanItem {
                                    title: i.title.clone(),
                                    summary: o.patch.summary.clone(),
                                    proven: o.proven,
                                    edits: o.patch.edits.clone(),
                                    test: o.test.clone(),
                                });
                                if o.proven {
                                    proven += 1;
                                    println!("  -> PROVEN: the regression test fails before the patch and passes after");
                                    // stack it: later issues are checked on top of this fix
                                    fix::apply_edits(&work, &o.patch.edits)?;
                                    if let Some((rel, content)) = &o.test {
                                        fix::save_test(&work, rel, content)?;
                                    }
                                    if apply {
                                        fix::apply_edits(&real, &o.patch.edits)?;
                                        if let Some((rel, content)) = &o.test {
                                            fix::save_test(&real, rel, content)?;
                                            println!("  -> saved {rel}");
                                        }
                                        println!("  -> applied to repo");
                                    }
                                } else {
                                    // The static checks and the gate are model-assisted and can be wrong: no fix that a
                                    // failing-then-passing test does not back is trusted, stacked or applied by default.
                                    let guarded = o.checks.iter().any(|c| c.name.starts_with("behavior guard") && c.passed);
                                    println!(
                                        "  -> {}",
                                        if guarded {
                                            "a behavior guard confirmed ordinary use still works, but the bug itself is not proven fixed."
                                        } else {
                                            "no behavior guard backs this patch either."
                                        }
                                    );
                                    println!("  -> UNPROVEN SUGGESTION: it passed the static checks and the patch review, but no regression test");
                                    println!("     fails before it and passes after it. It is saved in the plan, not stacked on later fixes, and");
                                    println!("     `apply-plan` skips it unless you pass --include-unproven. Read the diff yourself first.");
                                }
                            } else {
                                println!("  -> NOT verified; not applied");
                                deferred.push((n, j));
                            }
                        }
                        Err(e) if e.to_string().contains("QUOTA_EXHAUSTED") => return Err(e),
                        Err(e) => {
                            println!("  could not produce a fix: {e}");
                            events::emit(
                                "outcome",
                                serde_json::json!({"title": i.title, "verified": false, "proven": false, "error": events::truncate(&e.to_string(), 300)}),
                            );
                            deferred.push((n, j));
                        }
                    }
                }
                if deferred.is_empty() {
                    break;
                }
                if round == 1 {
                    println!("\n--- retrying {} unverified issue(s) on top of the verified fixes ---", deferred.len());
                }
                queue = deferred;
            }
            println!(
                "\n{verified}/{} fixes verified, {proven} proven by a regression test ({} unproven suggestion(s)), {already} resolved by earlier fixes",
                confirmed.len(),
                verified - proven
            );
            if !plan.items.is_empty() {
                let path = real.join(".autoresolve").join("plan.json");
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, serde_json::to_string_pretty(&plan)?)?;
                println!(
                    "plan saved to {} ({proven} proven fix(es), {} unproven suggestion(s)). Review it, then apply the proven ones with:\n  cargo run -p autoresolve-cli -- apply-plan",
                    path.display(),
                    plan.items.len() - proven
                );
            }
            fix::clean_scratch(&real);
            events::emit(
                "run_end",
                serde_json::json!({
                    "verified": verified, "proven": proven, "resolved_earlier": already, "confirmed": confirmed.len(),
                    "calls_main": provider.calls(), "calls_worker": strong.calls(),
                    "elapsed_ms": started.elapsed().as_millis() as u64
                }),
            );
            eprintln!(
                "[usage] {} model calls (reviewer/skeptic/patch gate) + {} (tester/fixer)",
                provider.calls(),
                strong.calls()
            );
        }
        Cmd::Events { root, run, list, raw } => {
            let path = root.join(".autoresolve").join(events::LOG_FILE);
            let all = events::read_events(&path, None).with_context(|| "no event log yet (run `fix` or `review` first)")?;
            // writes ignore a closed pipe (`| head`) instead of panicking
            let mut stdout = std::io::stdout().lock();
            if list {
                for (id, n) in events::runs(&all) {
                    if writeln!(stdout, "{id}  {n} events").is_err() {
                        break;
                    }
                }
                return Ok(());
            }
            let Some(id) = run.or_else(|| events::runs(&all).last().map(|(r, _)| r.clone())) else {
                let _ = writeln!(stdout, "no events recorded");
                return Ok(());
            };
            let mine: Vec<_> = all.into_iter().filter(|e| e.run == id).collect();
            if raw {
                for e in &mine {
                    if writeln!(stdout, "{}", serde_json::to_string(e)?).is_err() {
                        break;
                    }
                }
            } else {
                let _ = write!(stdout, "{}", events::summarize(&mine));
            }
        }
        Cmd::ApplyPlan { root, plan, include_unproven } => {
            let real = root.canonicalize()?;
            let path = plan.unwrap_or_else(|| real.join(".autoresolve").join("plan.json"));
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {} (run `fix` first)", path.display()))?;
            let plan: Plan = serde_json::from_str(&text).context("plan file is malformed")?;
            let policy = Policy::load(&real)?;
            eprintln!("[policy] {}", policy.describe());
            eprintln!("[sandbox] {}", autoresolve_core::sandbox::describe());
            let skipped = fix::apply_plan(&real, &plan, &policy, include_unproven)?;
            let applied: Vec<&PlanItem> = plan.items.iter().filter(|p| p.proven || include_unproven).collect();
            println!("applied {} fix(es) from {}", applied.len(), path.display());
            for p in &applied {
                println!("  - [{}] {}", if p.proven { "proven" } else { "UNPROVEN, applied by request" }, p.summary);
            }
            if skipped > 0 {
                println!("{skipped} unproven suggestion(s) were NOT applied (no regression test backs them). To review them:");
                for p in plan.items.iter().filter(|p| !p.proven) {
                    println!("  - {}", p.summary);
                }
                println!("  apply them anyway, after reading the diffs, with: apply-plan --include-unproven");
            }
            // run the saved regression tests against the real repo
            let mut failed = 0;
            for p in &applied {
                if let Some((rel, _)) = &p.test {
                    let (ok, tail) = autoresolve_core::sandbox::run(&real, &format!("python3 {rel}"), true);
                    println!("  [{}] {rel}", if ok { "PASS" } else { "FAIL" });
                    if !ok {
                        failed += 1;
                        println!("{tail}");
                    }
                }
            }
            if failed > 0 {
                std::process::exit(1);
            }
            println!("review the result with: git --no-pager diff");
        }
        Cmd::Tui { root, run } => tui::run(&root, run)?,
        Cmd::Sandbox { cmd, dir, writable } => {
            let dir = dir.canonicalize()?;
            eprintln!("[sandbox] {}", autoresolve_core::sandbox::describe());
            let (ok, out) = autoresolve_core::sandbox::run(&dir, &cmd, !writable);
            print!("{out}");
            println!("\nexit: {}", if ok { "success" } else { "failure" });
            if !ok {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}