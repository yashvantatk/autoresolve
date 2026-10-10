# AutoResolve

**An autonomous AI code review and repair system in Rust that keeps only the fixes it can prove.**

> *"No AI claim is trusted until something other than that AI checks it."*

AutoResolve reviews Python code with specialized LLM agents, writes targeted patches, and executes them in an isolated Docker sandbox. Fixes are **only applied if a deterministic regression test fails before the patch and passes after it**, ordinary behavior remains guarded, and static checks pass without regressions.

---

## Highlights

- **Proven Fixes Only**: Fixes are never trusted on prose promises. A patch is accepted only when a generated regression test fails on the original code and passes on the patched code.
- **Contract-Aware Enforcement**: Deterministically extracts docstrings and function signatures to ensure patches adhere to documented specifications (0 false trust on benchmark).
- **Hard Sandbox Isolation**: Non-AI checks and regression tests execute in a network-disabled Docker container (read-only root, 512 MB RAM, 1 CPU, all Linux capabilities dropped).
- **Four Interfaces**:
  - **VS Code Extension**: Native visual interface with one-click sidebar runner, real-time agent progress bar, and native red/green diff review with one-click Accept/Reject.
  - **Terminal UI (`tui`)**: Interactive Ratatui interface to monitor live pipeline execution and replay past event logs.
  - **Model Context Protocol (MCP)**: JSON-RPC 2.0 stdio server exposing symbol graphs, call hierarchies, AST rules, and agent reviews.
  - **GitHub Action**: CI workflow that runs static tree-sitter anti-pattern scans and uploads SARIF reports with zero model quota.
- **Deterministic Controller**: Rust code controls state transitions and agent turns; language models never approve their own work or decide pipeline progression.
- **Empirically Evaluated**: Validated against a 22-case benchmark with hidden oracles (100% detection on single-bug suite, 85% multi-bug recall, 0 false alarms on clean controls, 0 false trust).

---

## Contents

1. [Architecture & Pipeline](#1-architecture--pipeline)
2. [The Seven Specialized Roles](#2-the-seven-specialized-roles)
3. [The Trust Model: "Proven" vs "Correct"](#3-the-trust-model-proven-vs-correct)
4. [Interfaces](#4-interfaces)
   - [VS Code Extension](#vs-code-extension)
   - [CLI Reference](#cli-reference)
   - [Terminal UI (`tui`)](#terminal-ui-tui)
   - [MCP Server (`mcp`)](#mcp-server-mcp)
   - [GitHub Action & SARIF](#github-action--sarif)
5. [Quick Start](#5-quick-start)
6. [Configuration & Policy Engine](#6-configuration--policy-engine)
7. [Benchmark & Empirical Evaluation](#7-benchmark--empirical-evaluation)
8. [Repository Map](#8-repository-map)
9. [Key Lessons in Agent Design](#9-key-lessons-in-agent-design)

---

## 1. Architecture & Pipeline

AutoResolve avoids autonomous swarms in favor of a **deterministic state machine** written in Rust.

```
                  Source Code & AST Scan (Tree-sitter)
                                   │
                                   ▼
          Reviewer Agent(s) ──► Candidate Bug Reports
                                   │
                                   ▼
            Skeptic Agent ────► Refutation / Confirmation Check
                                   │
                                   ▼
         Contract Extractor ──► Extracts signature & docstrings
                                   │
        ┌──────────────────────────┴──────────────────────────┐
        ▼                                                     ▼
  Tester Agent                                         Guard Writer Agent
  Writes regression test                               Writes behavior guard
  (Must FAIL on buggy code)                            (Must PASS on original code)
        │                                                     │
        └──────────────────────────┬──────────────────────────┘
                                   │
                                   ▼
                              Fixer Agent
                      Proposes exact-once edits
                                   │
                                   ▼
                    Isolated Docker Sandbox Execution
               ├── Syntax check & tree-sitter rules
               ├── Ruff (E9, F rules) & Bandit (medium+)
               ├── Mypy static type verification
               ├── Regression test (Must now PASS)
               ├── Behavior guard (Must STILL pass)
               └── Optional user test command
                                   │
                                   ▼
                            Patch Gate Agent
                   Answers 4 booleans against contract
               (Rust code computes deterministic verdict)
                                   │
                   ┌───────────────┴───────────────┐
                   ▼                               ▼
             [PROVEN FIX]                 [UNPROVEN SUGGESTION]
       Regression test fail ──► pass       No proof of defect removal
       Saved to plan.json & applied        Saved for manual review only
```

---

## 2. The Seven Specialized Roles

To prevent models from rubber-stamping their own hallucinations, responsibility is partitioned across seven distinct roles divided into two tiers:

| Role | Tier | Output Format | Verification Mechanism (Non-Model) |
|---|---|---|---|
| **Reviewer** | Main | `submit_findings` | AST hints from tree-sitter; deduplication & voting quorum in code |
| **Skeptic** | Main | `submit_verdict` | Must cite explicit line numbers or reachability guards to confirm |
| **Still-Present** | Main | `submit_verdict` | Runs before subsequent stacked fixes to verify defect was not already cured |
| **Tester** | Worker | `submit_test` | Must FAIL on buggy code with expected failure; prohibited from inspecting implementation (`inspect`, `__defaults__`, reading source) |
| **Guard Writer**| Worker | `submit_guard` | Must PASS on original code; verifies normal production call paths |
| **Fixer** | Worker | `submit_patch` | Exact-once search/replace blocks; validated against `policy.toml` file/line limits |
| **Patch Gate** | Main | `submit_verdict` | Answers 4 booleans (`still_has_defect`, `unrelated_changes`, `changes_normal_behavior`, `contradicts_documented_contract`). **Rust code computes accept/reject.** |

- **Main Tier** (`AUTORESOLVE_MODEL`): Used for roles whose mistakes are silent (Reviewer, Skeptic, Gate). Default: `gemini-3.1-flash-lite`.
- **Worker Tier** (`AUTORESOLVE_MODEL_STRONG`): Used for roles whose mistakes are caught by the sandbox test suite (Tester, Guard, Fixer). Default: `gemini-3.5-flash-lite`.

---

## 3. The Trust Model: "Proven" vs "Correct"

A fix is **proven** when an independent regression test transitions from failing to passing while normal behavior invariants hold. 

However, **proven does not inherently mean correct**. In early benchmarks (`bare_except`), a model replaced a bare `except:` with `except (ValueError, TypeError):`. The generated test checked that `SystemExit` propagated, which passed. But the function's docstring specified: *"Return None when text is not a valid integer string. Other errors must propagate."* Catching `TypeError` broke calls like `parse_int(None)`.

### The Contract-Aware Fix
AutoResolve eliminates this gap through deterministic contract awareness:
1. **Contract Extraction**: Automatically extracts the function's signature and docstring via AST.
2. **Contract Injection**: Surfaces the documented specification to the Tester, Guard Writer, Fixer, and Patch Gate.
3. **Deterministic Gate Enactment**: The Gate evaluates `contradicts_documented_contract`. If any documented guarantee is violated, the patch is rejected in code.
4. **Result**: Benchmark false trust dropped to **0**.

---

## 4. Interfaces

AutoResolve provides four synchronized interfaces over the same core engine.

### VS Code Extension

Located in [`extension/`](extension/), this TypeScript extension brings the full agentic repair engine into the editor:

- **Sidebar Control**: Click the shield icon in the Activity Bar to inspect active files and trigger one-click analysis.
- **Live Progress & Agent Telemetry**: Watches `.autoresolve/events.jsonl` in real time, advancing through Reviewer, Skeptic, Tester, Guard, Fixer, and Gate stages to 100% completion.
- **Native Red/Green Diff Review**: Renders proven patches from `.autoresolve/plan.json` in VS Code's native diff editor (`vscode.diff`) via a virtual document provider (`autoresolve-preview://`).
- **One-Click Patch Application**: Review diffs side-by-side and click **Accept Fix** to apply search-and-replace edits, or **Reject Fix** to discard.
- **Fast AST Scanner**: Run instant anti-pattern checks directly through the MCP client without spending API quota.

```bash
cd extension
npm install
npm run compile
# Press F5 in VS Code / Antigravity IDE to launch Extension Development Host
```

### CLI Reference

The command-line binary `autoresolve-cli` provides direct pipeline access:

| Command | Description | Model Calls? |
|---|---|---|
| `scan [path] [--format sarif\|json\|text]` | Run deterministic AST anti-pattern rules (mutable defaults, bare except, `== None`) | No |
| `index [path]` | Extract functions, classes, and calls into SQLite call graph | No |
| `symbols` / `callers <name>` / `callees <name>` | Query symbol index and call relationships | No |
| `review <file> [--reviewers 3] [--votes N]` | Multi-agent review with specialist reviewers and skeptics | Yes |
| `fix <file> [--apply] [--test-cmd CMD]` | Full review + verify + fix pipeline; builds `.autoresolve/plan.json` | Yes |
| `fix <file> --issue "description"` | Issue-driven mode: skip reviewer/skeptic and fix user-specified bug | Yes |
| `apply-plan [--include-unproven]` | Re-verify policy and apply saved plan edits to repository | No |
| `events [--list] [--run ID]` | Print per-role telemetry, timing, and token usage breakdown | No |
| `tui` | Launch full-screen terminal UI monitor | No |
| `mcp` | Start Model Context Protocol server over stdio | Only `review` tool |

### Terminal UI (`tui`)

Built with Ratatui, `autoresolve-cli tui` reads `.autoresolve/events.jsonl` without touching model quota:

- **Live Stream & Replay**: Follow a running fix live or replay finished runs (`space` to pause, `+`/`-` speed).
- **Diff Inspection**: Formats proposed edits as colored diff blocks.
- **Role Breakdown**: Press `Tab` to toggle between event timelines and per-role call/cost metrics.
- **Forking (`x`)**: Export any confirmed issue into an executable `autoresolve fix --issue` command.

### MCP Server (`mcp`)

Implements Model Context Protocol (protocol version `2025-06-18`) over stdio JSON-RPC 2.0. Exposes read-only inspection tools to AI assistants (Claude Desktop, Cursor, Antigravity):
- `scan`: AST anti-pattern analysis.
- `symbols`: Repository symbol index.
- `callers` / `callees`: Name-based call hierarchy queries.
- `events_summary`: Timing and call statistics of latest run.
- `review`: Full multi-agent review.

### GitHub Action & SARIF

- **Action Definition**: [`action.yml`](action.yml)
- **Workflow**: [`.github/workflows/autoresolve.yml`](.github/workflows/autoresolve.yml)

Runs AST scans on every pull request and push to `main`, emitting SARIF 2.1.0 reports uploaded directly to GitHub Security Code Scanning with **zero model calls or API keys**.

---

## 5. Quick Start

### Prerequisites
- **Rust 1.90+** (edition 2024 via `rustup`)
- **Docker** (recommended for sandboxing; fallback to `AUTORESOLVE_SANDBOX=local`)
- **Node.js 20+** (for VS Code extension)
- **API Key**: Gemini API key (free tier works) or local Ollama instance

### 1. Build & Test
```bash
git clone https://github.com/your-username/autoresolve.git && cd autoresolve
cargo build --release
cargo test              # 66 tests passing (62 core, 4 cli)
```

### 2. Build Sandbox Image
```bash
docker build -t autoresolve-sandbox -f docker/sandbox.Dockerfile docker/
```

### 3. Environment Setup
```bash
export AUTORESOLVE_PROVIDER=gemini
export AUTORESOLVE_MODEL=gemini-3.1-flash-lite
export AUTORESOLVE_PROVIDER_STRONG=gemini
export AUTORESOLVE_MODEL_STRONG=gemini-3.5-flash-lite
export AUTORESOLVE_RPM=12
export AUTORESOLVE_DOCKER_IMAGE=autoresolve-sandbox
export GEMINI_API_KEY="your-api-key"
```

### 4. Run First Fix
```bash
./target/release/autoresolve-cli fix bench/cases/bare_except/parsing.py
```

---

## 6. Configuration & Policy Engine

### `policy.toml`
Place in repository root to enforce strict boundaries on model edits:

```toml
protected_paths     = ["tests/**", "setup.py", "alembic/**"]
max_files_per_patch = 3
max_changed_lines   = 40
```
Unknown keys trigger immediate errors to prevent configuration typos from silently disabling safety limits. `.git/`, `.autoresolve/`, and `autoresolve_regression/` are unconditionally protected.

### Sandbox Options
- `AUTORESOLVE_SANDBOX=docker` (default): Locked-down container (`--network none`, `--read-only`, `--cap-drop ALL`, 512 MB memory limit, 1 CPU).
- `AUTORESOLVE_SANDBOX=local`: Runs directly on host machine (unisolated; use with caution).

---

## 7. Benchmark & Empirical Evaluation

AutoResolve includes a built-in benchmark harness ([`bench/run_bench.py`](bench/run_bench.py)) with **22 seeded Python cases** evaluated against hidden oracles:
- **13 single-bug cases**: off-by-one, mutable default (visible & unobservable), `None` handling, operator bugs, zero division, `is` vs `==`, shell injection, SQL injection, dictionary mutation during iteration, closure late binding, bare `except`.
- **3 multi-bug cases**: 9 combined issues evaluating multi-defect recall.
- **6 clean controls**: completely bug-free implementations evaluating false alarm rates.

```bash
python3 bench/run_bench.py selfcheck   # Verifies all 22 cases and oracles (free)
python3 bench/run_bench.py compare     # Compares measured benchmark runs
```

### Measured Results Summary

| Metric | Baseline Arm (Single-Bug Suite) | Multi-Bug & Clean Controls | Contract-Aware Arm (`v4-single`) |
|---|---|---|---|
| **Bugs Detected (Recall)** | **13 / 13 (100%)** | **5 / 6 (83%)** | **6 / 7 (85%)** |
| **Strict Pass@1 (Proven Only)** | **10 / 13 (76%)** | **4 / 6 (66%)** | **4 / 7 (57%)** |
| **False Trust (Proven but Wrong)**| 1 (pre-contract `bare_except`) | 0 | **0 (Zero)** |
| **False Alarms on Clean Code** | **0 / 2 (0%)** | **0 / 4 (0%)** | **0 / 6 (0%)** |
| **Clean Code Breakage** | **0** | **0** | **0** |
| **Mean Model Calls per Case** | 20.7 | 21.5 | 49.3 |
| **Median Wall Clock Time** | 107 s | 21 s | 319 s |

*Note: All numbers measured via `python3 bench/run_bench.py compare`. No unmeasured estimates.*

---

## 8. Repository Map

```
autoresolve/
├── crates/
│   ├── core/                  # Core library (autoresolve-core)
│   │   ├── src/agent.rs       # Agent loop, tool dispatch, loop guards
│   │   ├── src/detectors.rs   # Tree-sitter AST anti-pattern rules
│   │   ├── src/graph.rs       # SQLite symbol index and call graph
│   │   ├── src/llm.rs         # Provider traits, Gemini/Ollama clients, RPM pacing
│   │   ├── src/review.rs      # Reviewer, specialist ensembles, skeptics
│   │   ├── src/fix.rs         # Tester, guard writer, fixer, contract extraction, gate
│   │   ├── src/sandbox.rs     # Docker isolation runner
│   │   ├── src/policy.rs      # Policy engine, path guards, symlink defenses
│   │   ├── src/lint.rs        # Ruff, Bandit, and Mypy sandbox verifiers
│   │   ├── src/report.rs      # Markdown and SARIF 2.1.0 formatters
│   │   └── src/events.rs      # JSONL event logger & per-role telemetry
│   └── cli/                   # CLI binary (autoresolve-cli)
│       ├── src/main.rs        # CLI subcommands and pipeline orchestrator
│       ├── src/tui.rs         # Ratatui terminal UI
│       └── src/mcp.rs         # MCP stdio JSON-RPC server
├── extension/                 # VS Code Extension (TypeScript)
│   ├── src/extension.ts       # Extension activator and commands
│   ├── src/pipelineRunner.ts  # CLI pipeline spawner and events.jsonl tailer
│   ├── src/sidebarProvider.ts # Sidebar Webview UI and progress bar
│   ├── src/diffViewer.ts      # Native VS Code diff viewer & edit applicator
│   └── src/mcpClient.ts       # TypeScript stdio MCP client
├── docker/
│   └── sandbox.Dockerfile     # Minimal Python 3.12 sandbox container
├── bench/                     # 22-case benchmark suite
│   ├── cases/                 # Seeded cases with hidden oracles
│   └── run_bench.py           # Scoring runner and comparison engine
├── .github/workflows/         # GitHub CI workflow
│   └── autoresolve.yml        # SARIF scan & upload action
├── action.yml                 # Composite GitHub Action
└── requirements.txt           # Python verification tooling (pytest, ruff, bandit, mypy)
```

---

## 9. Key Lessons in Agent Design

1. **A prompt is a suggestion; code is a guarantee.** If a constraint matters (file limits, path escapes, gate verdicts), enforce it deterministically in Rust.
2. **Proven is not correct without contracts.** A test can pass while violating unobserved caller assumptions. Surfacing signatures and docstrings to both the tester and the gate is essential to preventing false trust.
3. **Never allow models to inspect implementation in tests.** Reject generated tests that read `__defaults__`, `inspect`, or source files; they test the current code rather than intended behavior.
4. **Behavior guards are as critical as regression tests.** An AI fix can cure a bug while breaking every valid call path (e.g., shell injection fixes that break normal arguments). Requiring a pre-fix guard test prevents regressions.
5. **Separate main models from worker models.** Roles whose failures are silent (Skeptic, Gate) require the strongest available reasoning tier. Roles whose outputs are caught by compilers and test runners (Tester, Fixer) can utilize fast, inexpensive models.