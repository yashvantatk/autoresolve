use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "autoresolve", version, about = "Agentic code review & repair")]
struct Cli {
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
    },
}

fn main() -> Result<()> {
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
        Cmd::Scan { path } => {
            let mut findings = Vec::new();
            for entry in ignore::WalkBuilder::new(&path).build() {
                let entry = entry?;
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) != Some("py") {
                    continue;
                }
                let Ok(src) = std::fs::read_to_string(p) else { continue };
                findings.extend(autoresolve_core::detectors::scan_python(p, &src)?);
            }
            for f in &findings {
                println!("{}:{}:{}  [{}] {}", f.file.display(), f.line, f.col, f.rule, f.message);
            }
            println!("\n{} finding(s)", findings.len());
            if !findings.is_empty() {
                std::process::exit(1); // non-zero exit so CI can fail on findings
            }
        }
    }
    Ok(())
}