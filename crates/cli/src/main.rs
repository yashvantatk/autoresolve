use anyhow::{Context, Result};
use autoresolve_core::agent::Tools;
use autoresolve_core::review;
use autoresolve_core::graph::{self, FileGraph, Graph};
use autoresolve_core::llm::Gemini;
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
    }
    Ok(())
}