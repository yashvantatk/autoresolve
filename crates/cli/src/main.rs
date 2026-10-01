use anyhow::{Context, Result};
use autoresolve_core::agent::Tools;
use autoresolve_core::review;
use autoresolve_core::graph::{self, FileGraph, Graph};
use autoresolve_core::llm::Gemini;
use autoresolve_core::fix;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "autoresolve", version, about = "Agentic code review & repair")]
struct Cli {
    /// Where the repo graph database lives
    #[arg(long, global = true, default_value = ".autoresolve/graph.db")]
    db: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the syntax tree of a Python file
    Ast { file: PathBuf },
    /// Scan a directory for anti-patterns
    Scan {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit findings as JSON (for CI and tooling)
        #[arg(long)]
        json: bool,
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
    /// Agentic code review of a file (needs GEMINI_API_KEY)
    Review {
        file: PathBuf,
        /// Repository root the agent may read from
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Maximum agent steps before giving up
        #[arg(long, default_value_t = 12)]
        max_steps: usize,
    },
        /// Review a file, then generate and verify a fix for each confirmed issue
    Fix {
        file: PathBuf,
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Command to run in the sandbox after patching, e.g. "pytest -q"
        #[arg(long)]
        test_cmd: Option<String>,
        /// Write verified patches into the real repo (default is a dry run)
        #[arg(long)]
        apply: bool,
        #[arg(long, default_value_t = 12)]
        max_steps: usize,
    },
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
        Cmd::Scan { path, json } => {
            let mut findings = Vec::new();
            for p in python_files(&path)? {
                let Ok(src) = std::fs::read_to_string(&p) else { continue };
                findings.extend(autoresolve_core::detectors::scan_python(&p, &src)?);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&findings)?);
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
        Cmd::Review { file, root, max_steps } => {
            let provider = Gemini::from_env()?;
            index_repo(&root, &cli.db)?; // always review against a fresh graph
            let graph = Graph::open(&cli.db)?;
            let tools = Tools::new(&graph, &root)?;
            let judged = review::review(&provider, &tools, &file.display().to_string(), max_steps).await?;
            for (label, want) in [("CONFIRMED", "confirmed"), ("UNCERTAIN", "uncertain"), ("REFUTED by skeptic", "refuted")] {
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
        }
        Cmd::Fix { file, root, test_cmd, apply, max_steps } => {
            let provider = Gemini::from_env()?;
            index_repo(&root, &cli.db)?;
            let graph = Graph::open(&cli.db)?;
            let tools = Tools::new(&graph, &root)?;
            let target = file.display().to_string();
            let judged = review::review(&provider, &tools, &target, max_steps).await?;
            let confirmed: Vec<_> = judged.into_iter().filter(|j| j.verdict.verdict == "confirmed").collect();
            println!("\n{} confirmed issue(s) to fix", confirmed.len());

            let (mut verified, mut proven) = (0, 0);
            for (n, j) in confirmed.iter().enumerate() {
                let i = &j.issue;
                println!("\n=== [{}] {}:{}  {} ===", i.severity.to_uppercase(), i.file, i.line, i.title);

                // reproduction first: a test that must FAIL on the current code
                let slug = fix::slugify(&i.title, n);
                let repro = match fix::reproduce(&provider, &tools, i, &slug, max_steps).await {
                    Ok(t) => {
                        println!("regression test: {} (fails on the current code, as it should)", t.description);
                        Some(t)
                    }
                    Err(e) => {
                        println!("could not reproduce the bug with a test ({e}); the fix will be unproven");
                        None
                    }
                };

                let attempt = fix::fix_issue(
                    &provider,
                    &tools,
                    i,
                    test_cmd.as_deref(),
                    repro.as_ref().map(|t| (slug.as_str(), t)),
                    &format!("fix{n}"),
                    max_steps,
                )
                .await;
                match attempt {
                    Ok(o) => {
                        println!("{}", o.patch.summary);
                        print!("{}", o.diff);
                        for c in &o.checks {
                            println!("  [{}] {} {}", if c.passed { "PASS" } else { "FAIL" }, c.name, c.detail);
                        }
                        if o.verified {
                            verified += 1;
                            if o.proven {
                                proven += 1;
                                println!("  -> PROVEN: the regression test fails before the patch and passes after");
                            } else {
                                println!("  -> verified by static checks only (no regression test)");
                            }
                            if apply {
                                fix::apply_edits(tools.root(), &o.patch.edits)?;
                                if let Some((rel, content)) = &o.test {
                                    fix::save_test(tools.root(), rel, content)?;
                                    println!("  -> saved {rel}");
                                }
                                println!("  -> applied to repo");
                            }
                        } else {
                            println!("  -> NOT verified; not applied");
                        }
                    }
                    Err(e) => println!("  could not produce a fix: {e}"),
                }
            }
            println!("\n{verified}/{} fixes verified, {proven} proven by a regression test", confirmed.len());
        }
    }
    Ok(())
}