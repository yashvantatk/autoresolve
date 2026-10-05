# AutoResolve

**A Rust command-line tool that reviews Python code with AI agents, fixes the bugs it finds, and keeps only the fixes it can prove.**

> No AI claim is trusted until something other than that AI checks it.

Status date: 5 October 2026. Everything in this file is either measured, verified by a command you can run, or explicitly marked as *not measured*, *untested* or *planned*. If you add a number to a resume, a paper or a README, it must come from `python3 bench/run_bench.py compare`.

---

## Contents

1. [What it is, in plain words](#1-what-it-is-in-plain-words)
2. [How it helps a developer](#2-how-it-helps-a-developer)
3. [The key idea: "proven" is not "correct"](#3-the-key-idea-proven-is-not-correct)
4. [How it works](#4-how-it-works)
5. [Quick start](#5-quick-start)
6. [Command reference](#6-command-reference)
7. [Interfaces: terminal UI, MCP server, GitHub Action, SARIF](#7-interfaces)
8. [Configuration](#8-configuration)
9. [Repository map](#9-repository-map)
10. [The benchmark and its results](#10-the-benchmark-and-its-results)
11. [Current progress](#11-current-progress)
12. [Known limitations and observed failures](#12-known-limitations-and-observed-failures)
13. [Further work](#13-further-work)
14. [Handover guide for the next developer](#14-handover-guide-for-the-next-developer)
15. [Lessons that shaped the design](#15-lessons-that-shaped-the-design)
16. [Glossary](#16-glossary)

---

## 1. What it is, in plain words

You point AutoResolve at a Python file or repository. A team of AI agents, each with one narrow job, then does this:

1. **Reviewers** read the code and report suspected bugs.
2. **Skeptics** try to disprove each report. Only reports that survive continue.
3. For each surviving bug, a **tester** writes a regression test that must *fail* on the current code, for the reason the bug report predicts.
4. A **guard writer** writes a second test of *ordinary* use that must pass now and still pass after the fix.
5. A **fixer** proposes a patch as exact search-and-replace edits.
6. A pile of **non-AI checks** runs inside a locked-down Docker container: syntax, linters, type checker, security scanner, the regression test, the guard, your own test command, and a policy file that limits what a patch may touch.
7. A **gate** agent answers three yes/no questions about the patch, and **Rust code, not the model**, decides accept or reject from those answers.
8. A fix is **proven** only if its regression test went from failing to passing. Proven fixes go into a plan. Fixes without that proof are saved as *unproven suggestions* and are never applied by default.

Nothing is written to your repository until you run `apply-plan` (or pass `--apply`). The default is a dry run.

It is a **multi-agent pipeline run by a deterministic controller**. The Rust code decides which agent runs next. It is deliberately *not* an autonomous swarm, because language models skip steps, loop, and approve their own mistakes.

---

## 2. How it helps a developer

### The problem

AI coding tools are good at *proposing* bug fixes and bad at *knowing when they are wrong*. They write a patch, say "this fixes the bug", and the developer has to decide whether to believe it. In practice the failures are quiet:

- A model approved `subprocess.run(cmd, shell=True)` and `check_output(cmd_string, shell=False)` as "fixes" for shell injection. The second one breaks every normal call. Static checks alone passed both.
- A model fixed a bare `except:` by catching `(ValueError, TypeError)`, while the function's docstring said other errors must propagate. The generated test only covered part of the contract, so the fix was marked *proven* and was still wrong.
- Models write tests that can never fail, or that can never pass, and then report success.

### What AutoResolve gives you

| You get | How |
|---|---|
| **Evidence, not promises** | Every applied fix comes with a regression test that failed before and passes after. You can read the test. |
| **Safe by default** | Dry run. Unproven fixes are never applied. Policy file limits files and lines per patch and protects paths you name. Apply is all-or-nothing and rehearsed on a scratch copy first. |
| **Isolation** | Generated tests run in Docker with no network, a read-only root filesystem, all capabilities dropped, 512 MB of memory and 1 CPU. |
| **Auditability** | Every model turn, tool call, check and decision is written to `.autoresolve/events.jsonl`. A terminal UI replays any run. A per-role table shows where time and model calls went. |
| **Honest uncertainty** | When it cannot prove a fix, it says so and leaves the code alone. For bugs that cannot be observed through function calls (for example a shared default list that is never returned), "unproven" is the correct answer. |
| **Fits existing workflows** | SARIF output for GitHub code scanning, markdown for pull-request comments, an MCP server so AI assistants can query the repo graph and findings, an issue-driven mode that fixes a bug *you* describe. |

### When to use it

- Review a module or pull request and get findings that have already been challenged by a second agent.
- Turn a bug report into a reproducing test plus a candidate patch (`fix --issue`).
- Gate AI-generated changes in CI: run the free `scan` rules on every pull request and upload SARIF.
- Give an AI assistant (through MCP) read-only access to symbols, callers, callees and findings.

### When *not* to use it

- As a replacement for human review. "Proven" means the claimed case is fixed. It does **not** mean the patch is right (see the next section). Always read the diff.
- On languages other than Python (only Python is parsed today).
- On large repositories without testing first. The benchmark is small and easy; real-world rates will be lower.

### A typical session

```bash
autoresolve-cli scan src/                         # free: AST rules only, no model calls
autoresolve-cli review src/billing.py             # agents review, skeptics challenge
autoresolve-cli fix src/billing.py --test-cmd "pytest -q"   # dry run: builds plan.json
autoresolve-cli tui                               # watch or replay the run, read the diffs
autoresolve-cli apply-plan                        # re-check policy, rehearse, write, run saved tests
```

---

## 3. The key idea: "proven" is not "correct"

A fix is **proven** when a generated regression test *failed on the original code and passes after the patch*. That is strong evidence the claimed case is fixed. It is not proof the patch is correct, because the test only covers what the test author thought of.

The benchmark found this gap directly. On the `bare_except` case the function says "return `None` when the text is not a valid integer string. Other errors must propagate." The tester wrote a test that only checked that `SystemExit` propagates. The fixer caught `(ValueError, TypeError)`. The test passed, the gate approved, the fix was **proven**, and the hidden oracle rejected it, because `parse_int(None)` now returns `None` instead of raising `TypeError`.

The root cause was not model quality: *no role was ever shown the function's docstring.* The fix (implemented, unit-tested, **not yet measured on the benchmark**) is a deterministic extraction of the enclosing function's signature and docstring, which is then given to the tester, fixer and gate as the specification, plus a fourth gate answer, `contradicts_documented_contract`, enforced in code. See `enclosing_contract` and `with_contract` in `crates/core/src/fix.rs`.

---

## 4. How it works

### Pipeline

```
scan (AST rules) --> hints ----+
repo graph (SQLite) -> tools --+
                               v
 Reviewer (1 generalist, or 3 parallel specialists: correctness / security / robustness,
           optionally repeated N times with voting)  --> merge findings
   --> Skeptics (one per issue, run at once) --> confirmed issues
        (fix --issue "text" skips these two steps and starts from your bug report)

 for each confirmed issue:
   still present?  (only if proven fixes were already stacked)
   --> Tester writes a regression test: must FAIL on the current code, the way the claim predicts
   --> Guard writer writes a "behavior guard": a test of ORDINARY use, must PASS on the current code
   --> Fixer proposes exact-once search/replace edits, applied in a sandbox copy
   --> policy check (protected paths, max files and changed lines)
   --> checks: syntax | no new anti-patterns | ruff (E9,F) + bandit (medium+) + mypy not worse |
               regression test now passes | behavior guard still passes | your --test-cmd
   --> patch gate (a model answers 4 booleans, CODE decides):
         still_has_defect, unrelated_changes, changes_normal_behavior, contradicts_documented_contract
   --> PROVEN (regression test fail->pass)?   yes: stacked in a staging copy, goes in the plan
                                              no : saved as an UNPROVEN SUGGESTION, never stacked

 plan.json  --> apply-plan: re-check policy, rehearse on a scratch copy, write,
                run the saved tests.  Unproven items are skipped unless --include-unproven.
```

### The seven LLM roles

| Role | Tier | Returns result via | What checks it (not a model) |
|---|---|---|---|
| Reviewer (generalist, or 3 specialists) | main | `submit_findings` | AST hints in the prompt; merge and voting rules in code |
| Skeptic | main | `submit_verdict` | only confirmed issues continue |
| Still-present checker | main | `submit_verdict` | runs only when proven fixes were already stacked |
| Tester | worker | `submit_test` | must fail before, for the claimed reason; may not inspect the implementation; must pass after the patch |
| Guard writer | worker | `submit_guard` | must pass on the current code and still pass after the patch |
| Fixer | worker | `submit_patch` | exact-once edits, policy limits, every check above |
| Patch gate | main | `submit_verdict` (4 booleans) | decision computed in code from the booleans |

> **Naming trap.** `AUTORESOLVE_PROVIDER` / `AUTORESOLVE_MODEL` select the **main** tier (reviewer, skeptic, still-present, gate). `AUTORESOLVE_PROVIDER_STRONG` / `AUTORESOLVE_MODEL_STRONG` select the **worker** tier (tester, guard, fixer), despite the name. It was not renamed because that would break existing shell exports. **If `AUTORESOLVE_MODEL_STRONG` is unset, the worker tier silently uses the main model.** This happened in every benchmark run recorded so far (see section 10).

Why two tiers: a model whose mistakes tests catch (tester, fixer) can be cheap. A model whose mistakes are silent (reviewer, skeptic, gate) should be the strongest one available.

### Safety layers

1. The skeptic challenges every finding.
2. A regression test must fail before (for the stated reason) and pass after.
3. Tests that inspect the implementation (`__defaults__`, `inspect`, `ast`, reading source) are rejected. Exploit-succeeds tests are warned against.
4. The behavior guard: ordinary use must keep working.
5. The gate decision is made in code from four booleans, not read from prose.
6. Only proven fixes are stacked and applied by default.
7. ruff (errors only), bandit (medium and above) and mypy: a patch may not add findings.
8. `policy.toml`: protected paths, per-patch file and line limits. `.git/`, `.autoresolve/`, `policy.toml` and saved tests are always protected. Checked at patch time and again at apply time.
9. Path and symlink guards on every read and write.
10. Docker sandbox: no network, read-only root, all capabilities dropped, 512 MB, 1 CPU.
11. Agent-loop guards: repeated calls refused, terminal tool refused before any investigation, prose streaks forced into structured output, a model that never uses a tool is stopped after 4 prose answers.
12. Apply is all-or-nothing and rehearsed on a scratch copy first.
13. The documented contract (docstring) of the function under repair is given to the tester, fixer and gate. *Implemented, not yet measured.*

---

## 5. Quick start

### Prerequisites

| Need | Notes |
|---|---|
| Rust 1.90+ | edition 2024; install with rustup |
| Docker | runs the sandbox; without it you can set `AUTORESOLVE_SANDBOX=local` (**no isolation**) |
| Python 3.10+ | the benchmark runner uses only the standard library |
| A model | a Gemini API key (free tier works), or a local Ollama model |

### Build

```bash
git clone <this repo> && cd autoresolve
cargo build                     # binary: target/debug/autoresolve-cli
cargo test                      # expect 66 tests: 62 in core, 4 in cli
docker build -t autoresolve-sandbox -f docker/sandbox.Dockerfile docker/
```

`requirements.txt` lists the Python tools baked into the sandbox image (pytest, ruff, bandit, mypy). You only need to `pip install` it for `AUTORESOLVE_SANDBOX=local`.

### Settings

Put the non-secret settings in `~/autoresolve.env` and `source` it in every new terminal. **Export the API key by hand. Never store it in the repo and never paste it into a chat.**

```bash
# ~/autoresolve.env   (no secrets in this file)
export AUTORESOLVE_PROVIDER=gemini
export AUTORESOLVE_MODEL=gemini-3.1-flash-lite            # MAIN tier: reviewer, skeptic, still-present, gate
export AUTORESOLVE_PROVIDER_STRONG=gemini
export AUTORESOLVE_MODEL_STRONG=gemini-3.5-flash-lite     # WORKER tier: tester, guard, fixer
export AUTORESOLVE_RPM=12                                 # pace requests for 15-requests-per-minute models
export AUTORESOLVE_DOCKER_IMAGE=autoresolve-sandbox
export AUTORESOLVE_OLLAMA_MODEL=qwen3:4b
export AUTORESOLVE_OLLAMA_THINK=false
```

```bash
source ~/autoresolve.env
export GEMINI_API_KEY=...        # type it yourself
```

### First run

```bash
BIN=./target/debug/autoresolve-cli
$BIN scan bench/cases                          # free, no model calls
$BIN review bench/cases/bare_except/parsing.py # uses model quota (about 5 to 15 calls)
python3 bench/run_bench.py selfcheck           # free: verifies the 22 benchmark cases themselves
```

The CLI's binary is named `autoresolve-cli`. The terminal UI's "fork" key prints commands that start with `autoresolve`; create an alias (`alias autoresolve=/path/to/target/debug/autoresolve-cli`) or edit the command.

---

## 6. Command reference

| Command | What it does | Model calls? |
|---|---|---|
| `ast <file>` | print the syntax tree | no |
| `scan [path] [--format text\|json\|sarif] [--out FILE]` | tree-sitter rules: PY001 mutable default, PY002 bare `except`, PY003 `== None` | no |
| `index [path]` | build the SQLite symbol and call graph | no |
| `callers <name>` / `callees <name>` / `symbols` | query the graph (calls are matched **by name only**) | no |
| `review <file> [--format text\|json\|markdown\|sarif] [--out FILE] [--reviewers N] [--votes N] [--min-votes N]` | reviewer(s) plus skeptics | yes |
| `fix <file> [--test-cmd CMD] [--apply] [--max-steps N] [--reviewers N] [--votes N] [--min-votes N] [--issue "text"]` | full pipeline; dry run by default; builds `.autoresolve/plan.json` | yes |
| `fix <file> --issue "bug report"` | skip reviewer and skeptic and fix a bug you describe; if the text names a function in the file, its docstring becomes the contract | yes |
| `apply-plan [--plan FILE] [--include-unproven]` | re-check policy, rehearse on a scratch copy, write, run saved tests. Unproven items are skipped unless `--include-unproven` | no |
| `events [--list] [--run ID] [--raw]` | per-role table of model calls and time for a recorded run | no |
| `tui [--root DIR] [--run ID]` | terminal UI over the event log (live, replay, diffs) | no |
| `mcp` | MCP server on stdio (read-only tools) | only its `review` tool |
| `sandbox "<cmd>"` | run a command inside the sandbox (for testing isolation) | no |

Ensemble flags: `--reviewers 3` runs three specialist reviewers (correctness, security, robustness) in parallel and merges findings; `--votes N --min-votes M` repeats the review and keeps findings reported at least M times. Environment fallbacks: `AUTORESOLVE_REVIEWERS`, `AUTORESOLVE_VOTES`, `AUTORESOLVE_MIN_VOTES` (flags win).

---

## 7. Interfaces

### Terminal UI (`tui`)

Reads `.autoresolve/events.jsonl` and never calls a model or touches the repository. Start `fix` in one terminal and `tui` in another to watch it live.

| Key | Action |
|---|---|
| `j` / `k`, arrows, PageUp / PageDown | move through events |
| `n` / `p` | jump to next / previous issue |
| `g` / `G` | top / end |
| `f` | toggle follow (stick to the newest event) |
| `r` | replay the run from the start; `space` pause; `+` / `-` speed; `l` back to live |
| `Tab` | switch the right pane between event detail and the per-role summary |
| `J` / `K` | scroll the detail pane |
| `x` | **fork**: build `autoresolve fix '<file>' --issue '<bug report>'` for the issue under the cursor; it is printed when you quit with `q` |

The detail pane draws fixer patches as red/green diffs, shows tests and guards as code, findings with severity, and the gate's booleans in colour. The "fork" key only *prints a command*; re-running from inside the UI is not implemented.

### MCP server (`mcp`)

JSON-RPC 2.0 over stdio, one message per line. Read-only tools: `scan`, `symbols`, `callers`, `callees`, `events_summary`, and `review` (the only one that calls a model). `fix` and `apply-plan` are deliberately **not exposed** (a test checks this). Every path is confined to the directory the server started in. stdout carries only protocol messages; logs go to stderr.

Smoke test (verified, no model call):

```bash
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"scan","arguments":{"path":"bench/cases"}}}' \
 | ./target/debug/autoresolve-cli mcp 2>/dev/null
```

Client configuration (**untested with a real MCP client**; adjust to your client):

```json
{ "mcpServers": { "autoresolve": {
    "command": "bash",
    "args": ["-c", "cd /absolute/path/to/your/repo && /absolute/path/to/autoresolve-cli mcp"] } } }
```

### GitHub Action (`action.yml`, `.github/workflows/autoresolve.yml`)

Builds the CLI, runs `scan --format sarif` (no API key, no model calls) and uploads the result to GitHub code scanning. **Written but never run**: it can only be tested after the repository is pushed to GitHub. Place `action.yml` at the repository root and the workflow under `.github/workflows/`. Known detail: `scan` exits non-zero when it finds something, so the step ends with `|| true` and the findings reach the Security tab through the SARIF upload instead.

### SARIF and markdown

`scan` and `review` accept `--format sarif` (SARIF 2.1.0) and `review` also `--format markdown`, suitable for code-scanning tools and pull-request comments.

---

## 8. Configuration

### Environment variables

| Variable | Meaning |
|---|---|
| `GEMINI_API_KEY` | export by hand; never store or paste it |
| `AUTORESOLVE_PROVIDER`, `AUTORESOLVE_MODEL` | **main** tier: `gemini` or `ollama`, and the model code |
| `AUTORESOLVE_PROVIDER_STRONG`, `AUTORESOLVE_MODEL_STRONG` | **worker** tier (tester, guard, fixer). Unset means "same as main" |
| `AUTORESOLVE_OLLAMA_MODEL`, `AUTORESOLVE_OLLAMA_MODEL_STRONG`, `AUTORESOLVE_OLLAMA_URL` | local model settings |
| `AUTORESOLVE_OLLAMA_THINK=false` | leaving it unset once cost about 30 minutes per run |
| `AUTORESOLVE_NUM_CTX` | local model context size |
| `AUTORESOLVE_DOCKER_IMAGE` | sandbox image name (`autoresolve-sandbox`) |
| `AUTORESOLVE_SANDBOX` | `docker` (default) or `local` (no isolation) |
| `AUTORESOLVE_LINT=off` | disable ruff / bandit / mypy checks |
| `AUTORESOLVE_RPM` | space requests to one model at least 60/RPM seconds apart (use 12 for 15-RPM models) |
| `AUTORESOLVE_REVIEWERS`, `AUTORESOLVE_VOTES`, `AUTORESOLVE_MIN_VOTES` | reviewer ensemble defaults |
| `AUTORESOLVE_THINKING` | Gemini thinking level (see `llm.rs`) |

### `policy.toml` (optional, in the repository root)

```toml
protected_paths    = ["tests/**", "setup.py"]   # extra paths no patch may edit
max_files_per_patch = 3                          # default 3
max_changed_lines   = 40                         # default 40
```

Patterns match repo-relative paths with `/` separators; `*` does not cross `/`, use `**` to cross directories; matching is case-insensitive. **Unknown keys are an error**, so a typo cannot silently disable a limit.

### Gemini free-tier quota (counted per model, per day; resets at midnight Pacific time)

| Model | per minute | per day |
|---|---|---|
| gemini-3.1-flash-lite | 15 | 500 |
| gemini-3.5-flash-lite | 15 | 500 |
| other Flash models | 5 | 20 each |
| Pro models | not available on the key used | |
| Gemma 4 | 30 | 14,400, but only 16K tokens per minute (decided not to use) |

One simple fix costs about 20 requests; the full 22-case benchmark costs roughly 500. The runner stops by itself when a model's daily quota is exhausted and marks the case `QUOTA` (not scored).

---

## 9. Repository map

| Path | Purpose |
|---|---|
| `crates/core/src/agent.rs` | `run_agent` loop and `Tools` (list_symbols, get_callers, get_callees, read_lines, static_findings) |
| `crates/core/src/detectors.rs` | tree-sitter rules PY001, PY002, PY003 |
| `crates/core/src/graph.rs` | SQLite symbol and call graph |
| `crates/core/src/llm.rs` | `Provider` trait, Gemini and Ollama clients, retries, daily-quota detection, RPM pacing, `provider_from_env(strong)` |
| `crates/core/src/review.rs` | reviewer, skeptic, still-present, specialists, `merge_findings`, `review_with`, `ReviewOpts` |
| `crates/core/src/fix.rs` | tester (`reproduce`), guard (`write_guard`), `fix_issue`, patch gate, contract extraction, `apply_edits`, `apply_plan`, sandbox copies |
| `crates/core/src/sandbox.rs` | Docker runner (or local mode) |
| `crates/core/src/policy.rs` | `Policy`, `check_edits`, `check_plan`, `check_test_path`, symlink guards |
| `crates/core/src/lint.rs` | ruff, bandit, mypy before/after counts inside the sandbox |
| `crates/core/src/report.rs` | markdown and SARIF output |
| `crates/core/src/events.rs` | JSONL event log, role tagging, per-role summary |
| `crates/cli/src/main.rs` | CLI and the orchestrator |
| `crates/cli/src/tui.rs` | terminal UI |
| `crates/cli/src/mcp.rs` | MCP server |
| `docker/sandbox.Dockerfile` | sandbox image |
| `bench/` | benchmark: `cases/`, `run_bench.py`, `README.md`, `results/` |
| `.autoresolve/` | created at run time: graph db, `plan.json`, `events.jsonl`, scratch copies |

Contracts other code relies on: `trait Provider { complete, complete_json, calls }`, `run_agent(...)`, `review::review_with(...) -> Vec<Judged>`, `fix::fix_issue(...) -> Outcome`, `fix::apply_plan(...)`, the terminal tools `submit_findings / submit_verdict / submit_test / submit_guard / submit_patch`. Each agent call is wrapped in `events::scope("<role>", ...)` so events carry the role.

Event kinds in `events.jsonl`: `run_start`, `run_end`, `issue_start`, `model_turn`, `tool_call`, `terminal`, `terminal_forced`, `check`, `repro`, `guard`, `outcome`, `retry`, `model_error`, `paced`, `review_config`, `review_done`, `skipped_resolved`.

---

## 10. The benchmark and its results

### Design

`bench/` holds **22 small Python cases**, each with a hidden oracle test the agents never see:

- 13 single-bug cases (off-by-one, mutable default visible and unobservable, `None` handling, wrong operator, division by zero, `is` vs `==`, shell injection, SQL injection, dict mutation during iteration, closure late binding, bare `except`),
- 3 multi-bug files (9 bugs; they measure recall),
- 6 clean controls with no bug (they measure false alarms).

```bash
python3 bench/run_bench.py selfcheck                 # verify the cases (free, no models)
python3 bench/run_bench.py run --label NAME [--cases ...] [--max-calls N] [--bin ~/ar-bench-bin]
python3 bench/run_bench.py compare [--cases ...]     # one column per label
```

The runner copies only the buggy file into a scratch directory, runs `fix`, then scores two worlds with the hidden oracle: after `apply-plan` (proven fixes only) and after `apply-plan --include-unproven`. Each case's events, console log and plan are saved in `bench/results/<label>-<time>.events/`. **Always use a frozen binary copy** (`cp target/debug/autoresolve-cli ~/ar-bench-bin`) so rebuilding does not change a run in progress. `--max-calls` is a total budget for the run, checked between cases.

Metrics: bugs detected (recall); **strict pass@1** (proven and oracle-correct); **lenient pass@1** (including unproven); **false trust** (proven fixes beyond the bugs the oracle confirms as fixed; must be 0); unproven-but-correct; false alarms and breakage on clean code; time; model calls.

*Metric correction (5 Oct 2026):* false trust was first computed per case ("any proven fix in a case whose oracle fails"), which wrongly flagged a multi-bug case where the one proven fix was correct and the failure came from bugs that were never proven fixed. It is now `max(0, proven fixes - bugs the oracle confirms fixed)`, which equals the old rule for single-bug cases. `compare` recomputes it from saved results.

### Results so far

> **Configuration of every result below:** binary `bin2` (before the contract-aware changes), and **both model tiers were `gemini-3.1-flash-lite`**, because `AUTORESOLVE_MODEL_STRONG` was not set (the worker tier fell back to the main model; the saved `run_start` events show this). Any wording like "main + worker models" would be wrong for these runs.

**Run `baseline`: 15 cases, one run**

| Metric | Result |
|---|---|
| Bugs detected | 13 / 13 |
| Strict pass@1 | 10 / 13 |
| Lenient pass@1 | 11 / 13 |
| False trust | 1 (`bare_except`, root cause explained in section 3) |
| Unproven but correct | 1 / 1 (`mutable_default_hidden`, an unobservable bug) |
| False alarms on clean code | 0 of 2 controls |
| Median time per case | 107 s |
| Mean model calls per case | 20.7 (310 in total) |

**Run `base2`: 7 further cases, same configuration**

| Case | Outcome |
|---|---|
| `clean_cache`, `clean_dates`, `clean_retry`, `clean_table` | 0 false alarms, 3 to 4 calls each |
| `multi_inventory` | 3 of 3 bugs detected, proven and oracle-correct (59 calls, 215 s) |
| `multi_scheduler` | 2 of 3 bugs detected (missed `normalize_name`: `.lower` without parentheses); `average_wait` fix proven and correct; the `pick_slot` fix was wrong (returned `None` instead of fixing the off-by-one), failed to get a reproducing test, stayed **unproven and was not applied** (55 calls, 189 s) |
| `multi_textstats` | not scored: daily quota exhausted |

Combined for `base2`: 5 of 6 multi-bug bugs detected, 4 of 6 fixed with proof and oracle-correct, 0 false trust after the metric correction. Across all runs so far: 6 clean controls, 0 false alarms.

### Pending (not yet measured)

Two runs on the corrected two-model configuration (3.1 main, 3.5 worker) were started with `~/run_arms.sh` on 5 October 2026 and had not been read at the time of writing. Fill this table from `compare` output:

```bash
python3 bench/run_bench.py compare --cases bare_except multi_scheduler multi_textstats multi_inventory shell_injection mutable_default_hidden
```

| Metric | `v4-single` (1 reviewer) | `v4-spec3` (3 specialists) |
|---|---|---|
| Bugs detected | _pending_ | _pending_ |
| Strict pass@1 | _pending_ | _pending_ |
| False trust | _pending_ | _pending_ |
| False alarms (4 clean controls) | not run | _pending_ |
| Mean calls per case | _pending_ | _pending_ |

These runs also give the first measurement of the contract-aware changes on `bare_except` and `multi_scheduler`.

### How to read these numbers

One run, 13 to 22 simple cases written by an AI assistant, a non-deterministic reviewer. With 13 cases, "10 of 13" could truly be anywhere from about 50% to 92%. Real-world rates will be lower. These results support comparisons *between configurations*, not claims about real-world accuracy. Measure variance (repeat each arm several times) before drawing conclusions.

### Other measured data points

| Run | Time | Calls | Result |
|---|---|---|---|
| review, qwen3:4b, thinking off (early code) | 9m28s | 18 | 0 confirmed |
| review, gemini-3.1-flash-lite (early code) | 1m56s | 17 | 3 confirmed |
| fix, Gemini reviewer + qwen3:4b workers, `demo5` | 4m28s | 10 + 6 | proven, oracle-correct |
| fix, Gemini main + qwen3:4b workers, `demo6` | 24m21s | 22 + 29 | 0 of 1 verified |
| fix, all Gemini, `demo6` | 5 to 11 min | 40 to 70 | safe refusals, 0 proven (hard case) |
| bench `wrong_operator`, first run | 417 s | 47 | not fixed (gate rejected the fix) |
| bench `wrong_operator`, after gate wording change | 99 s | 18 | proven, oracle-correct (n=1; consistent with the cause, not proof) |
| `fix --issue` on `bare_except`, both tiers on `gemini-3.5-flash-lite` | n/a | 28 | **not verified, nothing applied** (see section 12) |

---

## 11. Current progress

"Verified" below means checked by a command, not assumed.

| Area | Status | How far it is verified |
|---|---|---|
| CLI, tree-sitter rules, SQLite call graph, Gemini and Ollama providers, agent loop | Done | Benchmark runs; 62 core tests |
| Reviewer, skeptic, tester, guard writer, fixer, gate, still-present checker | Done | Benchmark runs |
| Docker sandbox | Done | Four isolation tests (no network, read-only, host files hidden, local-mode contrast) |
| Policy engine, symlink and path guards | Done | Unit tests; enforced at patch time and apply time |
| ruff / bandit / mypy verification, SARIF, markdown | Done | Tests and runs |
| Unproven tier, gate decided in code, behavior guard | Done | Benchmark runs |
| Event log, per-role summary, quota pacing | Done | Runs |
| Specialist reviewers, voting, concurrent skeptics | Done | Builds; scripted-model tests pass; **benefit not yet measured** (`v4-spec3` pending) |
| Benchmark (22 cases, hidden oracles, runner, `compare`) | Done | `selfcheck` passes: all cases sound |
| Contract-aware tester, fixer and gate | Implemented | Two unit tests; **effect on the benchmark not yet measured** |
| `fix --issue` | Implemented | One live run: ended safely unverified (section 12); not benchmarked |
| Terminal UI (live, replay, diffs, fork command) | Implemented | Launched on a real log; two unit tests. Fork prints a command only |
| MCP server | Implemented | Protocol smoke test and two unit tests; **not tried with a real MCP client** |
| GitHub Action | Files written | **Never run** (needs a GitHub repository) |
| SWE-bench Lite subset | **Dropped** | Needs per-repository dependency images that do not fit a network-less sandbox, and about 1,500 model calls against a daily limit of about 1,000 per model. `fix --issue` is the issue-driven mode that replaces it |
| README demo GIF | Not done | |
| Test count | 66 pass | 62 in `autoresolve-core`, 4 in `autoresolve-cli` |

---

## 12. Known limitations and observed failures

**Observed in this project**

- **Proven is not correct.** `bare_except` produced a proven fix that the oracle rejected (section 3). The contract-aware mitigation is not yet measured.
- **The tester can write unsatisfiable tests.** In the `fix --issue` demo on `bare_except`, the test called `parse_int(SystemExit(1))`, but `int(SystemExit(1))` raises `TypeError`, so no possible fix could make the test pass. Both attempts stayed unverified, nothing was applied (the safe outcome), and 28 worker calls were spent. A rule that "repairs the test when the patch fails the same way" was considered and **rejected on purpose**: it can loosen a test until a wrong patch passes, which is exactly how false trust arises.
- **The reviewer can miss bugs and mis-describe them.** `multi_scheduler`: the missed `.lower` bug, and an imprecise "potential IndexError" for what is really an off-by-one. Specialist reviewers are meant to help; not yet measured.
- **Weak fixers ignore the contract.** Even with the docstring in the task, the demo fixer caught `TypeError`. That is why the test and the gate must enforce it, not the prompt.
- **A round-2 retry repeats identical work.** Unverified issues are retried "on top of the verified fixes" but the second round regenerated the same test and failed the same way.
- **The fixer sometimes leaks reasoning ("Wait, the prompt said...") into its summary text.**
- **The gate can approve nonsense in its reasoning even when the booleans are right.**

**Structural**

- The call graph matches calls by name only.
- The pipeline is sequential per issue; only reviewers and skeptics are concurrent.
- The sandbox copy skips hidden files (such as `.env`), so tests that need them will not find them.
- `--db` defaults to a path relative to the current directory.
- Only Python is supported.
- Small local models (qwen3:4b) skip investigation, answer in prose and write bad tests. They are a benchmark arm, not a working configuration.
- Quota decides what is affordable: about 500 calls per model per day on the free tier.
- Nothing has been run against a large real repository yet.

---

## 13. Further work

### Do first (small, high value)

1. **Read the pending benchmark arms** (section 10) and fill in the table. Re-run `bare_except` and `multi_scheduler` to measure the contract-aware changes.
2. **Warn at startup when `AUTORESOLVE_MODEL_STRONG` is unset** (or print both tiers' models in every run header). Missing settings silently changed the meaning of every result so far.
3. **Measure variance:** repeat each arm 3 times and report ranges.
4. **Try the MCP server with a real client** and run the GitHub Action on a pushed repository.
5. **Record a demo GIF** of one end-to-end fix with the terminal UI.

### Next (medium)

6. **Per-role model override** (for example `AUTORESOLVE_MODEL_GATE`) so only the gate uses a stronger model.
7. **Escalation routing:** retry a failed fix with a stronger model; measure cost per fix.
8. **Test satisfiability checks:** reject tests that no input could satisfy, without loosening them to fit a patch (see the rejected idea in section 12).
9. **Let the tester declare "not observable through calls"** instead of burning attempts; record it as its own benchmark category.
10. **Save the behavior guard** in the plan as a second regression test.
11. **Speed:** put the target file's source into the tester and fixer task, cap worker output length, run independent issues in parallel.
12. **`past_attempts` tool** so the fixer sees which patches were already rejected in this run; stop round 2 from repeating identical work.
13. **Clean up the fixer summary** so it stops leaking reasoning text.
14. **Real-world bug set:** 15 to 20 small bugs from open-source Python repositories, with the upstream fix commit as the oracle (the realistic replacement for SWE-bench Lite).
15. **In-UI fork:** let the terminal UI re-run the selected issue instead of printing a command.
16. **Benchmark per-bug attribution:** map each plan edit to a specific bug, instead of the current count-based false-trust rule.
17. **A policy-gated `fix` dry-run tool in the MCP server** (still never `apply-plan`).

### Later (large or research-style)

18. More detectors (deep nesting, O(n²) loops) and **blast-radius analysis** from the call graph.
19. A **better call graph** than name-only matching.
20. **More languages** (JavaScript, C++, Rust via tree-sitter) and a **Claude provider** for model comparison.
21. A **supervisor agent** experiment (a model chooses which specialists run), only with hard step, cost and permission limits enforced in Rust.
22. A **vector index** over the repository for retrieval; **Firecracker or gVisor** instead of plain Docker.
23. **Benchmark plots and a results dashboard.**
24. A proper **SWE-bench Lite** run, if per-repository images and enough model quota become available.

---

## 14. Handover guide for the next developer

### First hour

```bash
cargo build && cargo test                       # expect 66 passing (62 core + 4 cli)
docker build -t autoresolve-sandbox -f docker/sandbox.Dockerfile docker/
source ~/autoresolve.env                        # create it from section 5 first
python3 bench/run_bench.py selfcheck            # free
./target/debug/autoresolve-cli scan bench/cases # free
./target/debug/autoresolve-cli tui              # opens the latest recorded run (if any)
```

### Read in this order

1. This file.
2. `crates/core/src/events.rs` (what is recorded) and the saved events in `bench/results/*.events/`.
3. `crates/cli/src/main.rs` (the orchestrator: how the stages connect).
4. `crates/core/src/fix.rs` (`reproduce`, `write_guard`, `fix_issue`, gate, contract extraction, `apply_plan`). This is the heart of the trust model.
5. `crates/core/src/review.rs` and `agent.rs`.
6. `bench/run_bench.py` (scoring rules, in the comment at the top).

To see exactly what happened in any benchmark case:

```bash
python3 - <<'EOF'
import json, sys
for l in open("bench/results/<label>-<time>.events/<case>.events.jsonl"):
    e = json.loads(l)
    if e["kind"] in ("terminal", "check", "outcome", "repro", "guard", "issue_start"):
        print(e["role"], e["kind"], json.dumps(e["data"])[:400])
EOF
```

### Pitfalls that already cost time

- **The tier naming trap** (section 4): `*_STRONG` is the *worker* tier, and unset means "same as main".
- **Quota is per model per day**, resets at midnight Pacific time, and a case that dies on it is marked `QUOTA`, not failed. Watch for 429 responses.
- **Use a frozen binary for benchmark runs**, and note which binary and which settings produced each result. Record both in the results table.
- **Do not run two benchmark processes at once**; they share the same per-minute limit and skew each other.
- **Tests that use shared temp directory names are flaky**; give every test a unique directory name.
- **Never put the API key in a file inside the repository or in a chat.** If a key was ever pasted anywhere, delete it in Google AI Studio and create a new one.
- **Never write a number on a resume or in a report that did not come from `compare`.** State the configuration it was measured under.

### Resume wording (true as of today; extend only with measured numbers)

> Architected a Rust multi-agent CLI of seven role-specialized LLM agents (reviewer, skeptic, still-present checker, tester, behavior-guard writer, fixer, patch gate) under a deterministic controller, grounded in a tree-sitter and SQLite call graph with a replayable JSONL event log. Engineered a "proven fixes only" pipeline: a patch is trusted only if a generated regression test fails before and passes after; unproven patches are never auto-applied, and every patch must also pass a behavior-guard test and a gate whose decision is enforced in code. Sandboxed all test execution in network-disabled Docker containers with a TOML policy engine, backed by 66 tests. Built a 22-case seeded-bug benchmark with hidden oracles measuring recall, strict pass@1 and false trust, and a terminal UI and MCP server over the event log.

Add benchmark numbers only from `compare`, together with the configuration (both tiers `gemini-3.1-flash-lite` for the first runs).

---

## 15. Lessons that shaped the design

1. A prompt is a request. If a rule matters, enforce it in code.
2. "Verified" is only as strong as the checks. Static checks plus a model gate approved two wrong shell-injection "fixes"; that is why unproven fixes are never applied by default and why the behavior guard exists.
3. "Proven" means the claimed case is fixed, not that the fix is right (`bare_except`).
4. Models whose mistakes are silent (reviewer, skeptic, gate) deserve the strongest model.
5. Tests that copy the code under test, assert that the exploit works, or poke at `__defaults__` are worthless.
6. Some bugs cannot be observed through calls. For those, "unproven" is the honest result.
7. Reviewers are non-deterministic. Measure variance; one run proves little.
8. Per-model quota and per-minute limits decide what experiments are affordable.
9. Do not tune against one demo file. Build a benchmark.
10. A metric can be wrong too. The first false-trust definition mislabelled a correct fix; check what a number counts before believing it.
11. A missing setting can silently change what a result means (the worker model fell back to the main model in every recorded run).
12. Do not "repair" a test to fit a failing patch: that is how wrong fixes become proven.

---

## 16. Glossary

| Term | Meaning |
|---|---|
| **Proven fix** | a fix whose regression test failed before the patch and passes after it |
| **Unproven suggestion** | a fix that passed the other checks but has no failing-then-passing test; saved, never applied by default |
| **False trust** | a proven fix that the hidden oracle rejects (target: 0) |
| **Regression test** | a test written for one bug that must fail on the current code and pass after the fix |
| **Behavior guard** | a test of *ordinary* use that must pass both before and after the patch |
| **Gate** | the final model review of a patch; it answers booleans and Rust code decides |
| **Contract** | the signature and docstring of the function being fixed; the specification the fix must respect |
| **Skeptic** | an agent whose job is to refute a reported bug |
| **Main tier / worker tier** | the model used for reviewer, skeptic, still-present and gate / for tester, guard and fixer |
| **Oracle** | a hidden test in the benchmark that decides whether code is truly correct |
| **Strict / lenient pass@1** | a case counts as passed using proven fixes only / also counting unproven ones |
| **SARIF** | the standard JSON format for static-analysis results (used by GitHub code scanning) |
| **MCP** | Model Context Protocol, how AI assistants call external tools |
| **Event log** | `.autoresolve/events.jsonl`, an append-only record of every model turn, tool call, check and decision |