#!/usr/bin/env python3
"""AutoResolve seeded-bug benchmark.

Each case in bench/cases/<id>/ has:
  <module>.py  the buggy file the agents see (copied alone into a scratch directory)
  fixed.py     a reference fix (only used by `selfcheck`)
  oracle.py    a hidden test that decides whether the code is really correct. Agents never see it.
  case.json    file, category, seeded bug line(s), whether the bug is observable through calls

Commands (run from the repo root):
  python3 bench/run_bench.py selfcheck                 verify every case: oracle fails on the bug, passes on the fix
  python3 bench/run_bench.py run --label NAME          run the agent on every case and score it
  python3 bench/run_bench.py compare [files...]        compare result files side by side

Metrics (bug cases only unless noted):
  detected          a confirmed finding lands within 2 lines of a seeded bug line
  strict pass@1     after `apply-plan` (proven fixes only) the hidden oracle passes
  lenient pass@1    after `apply-plan --include-unproven` the hidden oracle passes
  false trust       a PROVEN fix was applied and the oracle still fails (the number that must stay at 0)
  unproven correct  of the cases that produced an unproven suggestion, how many the oracle accepts
  false alarms      (clean controls) confirmed findings on code that has no bug
  broke clean code  (clean controls) the oracle fails after applying everything
"""
import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

BENCH = Path(__file__).resolve().parent
CASES = BENCH / "cases"
RESULTS = BENCH / "results"
ROOT = BENCH.parent


def load_cases(only=None):
    out = []
    for d in sorted(CASES.iterdir()):
        if not (d / "case.json").exists():
            continue
        c = json.loads((d / "case.json").read_text())
        c["dir"] = d
        if only and c["id"] not in only:
            continue
        out.append(c)
    return out


def run_oracle(case, workdir):
    """(passed, per_bug) where per_bug maps a bug name to True/False for oracles that report `OK name` / `FAIL name`."""
    env = {**os.environ, "BENCH_DIR": str(workdir), "PYTHONPATH": str(workdir), "PYTHONDONTWRITEBYTECODE": "1"}
    r = subprocess.run([sys.executable, str(case["dir"] / "oracle.py")], cwd=workdir, env=env,
                       capture_output=True, text=True, timeout=120)
    detail = {}
    for line in r.stdout.splitlines():
        if line.startswith("OK "):
            detail[line[3:].strip()] = True
        elif line.startswith("FAIL "):
            detail[line[5:].strip()] = False
    return r.returncode == 0, detail


def oracle_passes(case, workdir):
    return run_oracle(case, workdir)[0]


def bugs_of(case):
    return case.get("bugs") or [{"name": case["id"], "line": n} for n in case["bug_lines"]]


def selfcheck(args):
    bad = 0
    for c in load_cases(args.cases):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            shutil.copy(c["dir"] / c["file"], t / c["file"])
            buggy_ok, buggy_detail = run_oracle(c, t)
            shutil.copy(c["dir"] / "fixed.py", t / c["file"])
            fixed_ok, fixed_detail = run_oracle(c, t)
        clean = c["category"] == "clean_control"
        ok = (buggy_ok and fixed_ok) if clean else ((not buggy_ok) and fixed_ok)
        if len(bugs_of(c)) > 1:  # multi-bug: each bug must fail alone on the buggy file and pass on the fix
            names = [b["name"] for b in bugs_of(c)]
            ok = ok and all(buggy_detail.get(n) is False for n in names) and all(fixed_detail.get(n) is True for n in names)
        bad += 0 if ok else 1
        print(f"{'ok ' if ok else 'BAD'}  {c['id']:<26} oracle on buggy: {'pass' if buggy_ok else 'fail'}, on fixed: {'pass' if fixed_ok else 'fail'}")
    print("all cases are sound" if not bad else f"{bad} case(s) are broken")
    return 1 if bad else 0


def read_events(work):
    path = Path(work) / ".autoresolve" / "events.jsonl"
    if not path.exists():
        return []
    evs = []
    for line in path.read_text().splitlines():
        try:
            evs.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    if not evs:
        return []
    last = evs[-1]["run"]
    return [e for e in evs if e["run"] == last]


def from_events(evs):
    """Numbers for one run, taken from the event log."""
    out = {"elapsed_s": None, "calls_main": 0, "calls_worker": 0, "confirmed_lines": [],
           "verified": 0, "proven": 0, "retries": 0, "retry_wait_s": 0, "paced_s": 0.0, "role_s": {},
           "failed_checks": [], "repro": [], "guard": []}
    for e in evs:
        d = e.get("data", {})
        k = e["kind"]
        if k == "run_end":
            out["elapsed_s"] = round(d.get("elapsed_ms", 0) / 1000, 1)
            out["calls_main"] = d.get("calls_main", 0)
            out["calls_worker"] = d.get("calls_worker", 0)
            out["verified"] = d.get("verified", 0)
            out["proven"] = d.get("proven", 0)
        elif k == "issue_start" and d.get("round", 1) == 1:
            out["confirmed_lines"].append(d.get("line", 0))
        elif k == "check" and not d.get("passed", True):
            out["failed_checks"].append(f"{d.get('name', '?')}: {str(d.get('detail', ''))[:200]}")
        elif k == "repro":
            out["repro"].append(bool(d.get("ok")))
        elif k == "guard":
            out["guard"].append(bool(d.get("ok")))
        elif k == "retry":
            out["retries"] += 1
            out["retry_wait_s"] += d.get("wait_s", 0)
        elif k == "paced":
            out["paced_s"] += d.get("wait_ms", 0) / 1000
        elif k == "model_turn":
            out["role_s"][e["role"]] = round(out["role_s"].get(e["role"], 0) + d.get("ms", 0) / 1000, 1)
    return out


def run_case(case, args):
    work = Path(tempfile.mkdtemp(prefix=f"bench-{case['id']}-"))
    shutil.copy(case["dir"] / case["file"], work / case["file"])
    row = {"case": case["id"], "category": case["category"], "observable": case["observable"],
           "label": args.label, "status": "ok", "env": {k: v for k, v in os.environ.items()
           if k.startswith("AUTORESOLVE_") and k != "AUTORESOLVE_API_KEY"}}
    t0 = time.time()
    try:
        r = subprocess.run([args.bin, "fix", case["file"], "--root", ".", "--test-cmd", case["test_cmd"]],
                           cwd=work, capture_output=True, text=True, timeout=args.timeout)
        text = (r.stdout or "") + (r.stderr or "")
        if getattr(args, "evdir", None):
            (args.evdir / f"{case['id']}.log").write_text(text)
            if (work / ".autoresolve" / "plan.json").exists():
                shutil.copy(work / ".autoresolve" / "plan.json", args.evdir / f"{case['id']}.plan.json")
        if "QUOTA_EXHAUSTED" in text:
            row["status"] = "quota"
        elif r.returncode != 0:
            row["status"] = "error"
            row["error"] = text[-400:]
    except subprocess.TimeoutExpired:
        row["status"] = "timeout"
    row["wall_s"] = round(time.time() - t0, 1)
    ev_file = work / ".autoresolve" / "events.jsonl"
    if ev_file.exists() and getattr(args, "evdir", None):
        shutil.copy(ev_file, args.evdir / f"{case['id']}.events.jsonl")
    row.update(from_events(read_events(work)))
    lines = row["confirmed_lines"]
    row["confirmed"] = len(lines)
    bugs = bugs_of(case)
    near = lambda l, b: abs(l - b["line"]) <= 2
    row["bugs_total"] = len(bugs)
    row["bugs_found"] = sum(1 for b in bugs if any(near(l, b) for l in lines))
    row["unmatched_findings"] = sum(1 for l in lines if not any(near(l, b) for b in bugs))
    row["detected"] = (row["bugs_found"] == len(bugs)) if bugs else None

    # score two worlds from the saved plan: proven fixes only, and everything including unproven
    for name, extra in (("proven_only", []), ("with_unproven", ["--include-unproven"])):
        copy = Path(tempfile.mkdtemp(prefix=f"bench-{case['id']}-{name}-"))
        shutil.copytree(work, copy, dirs_exist_ok=True, ignore=shutil.ignore_patterns("sandbox", "__pycache__"))
        a = subprocess.run([args.bin, "apply-plan", "--root", ".", *extra], cwd=copy,
                           capture_output=True, text=True, timeout=600)
        row[f"apply_{name}_ok"] = a.returncode == 0
        passed, detail = run_oracle(case, copy)
        row[f"oracle_{name}"] = passed
        row[f"bugs_fixed_{name}"] = sum(1 for b in bugs if detail.get(b["name"])) if detail else (1 if passed and bugs else 0)
        shutil.rmtree(copy, ignore_errors=True)
    shutil.rmtree(work, ignore_errors=True)
    return row


def summarize(rows):
    bug = [r for r in rows if r["category"] != "clean_control" and r["status"] == "ok"]
    clean = [r for r in rows if r["category"] == "clean_control" and r["status"] == "ok"]
    n = len(bug)
    pct = lambda a, b: f"{a}/{b}" + (f" ({100 * a // b}%)" if b else "")
    med = lambda xs: round(statistics.median(xs), 1) if xs else None
    unproven = [r for r in bug if r["verified"] > r["proven"]]
    tot = sum(r.get("bugs_total", 1) for r in bug)
    found = sum(r.get("bugs_found", 1 if r["detected"] else 0) for r in bug)
    fixed_p = sum(r.get("bugs_fixed_proven_only", 1 if r["oracle_proven_only"] else 0) for r in bug)
    fixed_a = sum(r.get("bugs_fixed_with_unproven", 1 if r["oracle_with_unproven"] else 0) for r in bug)
    extra = sum(r.get("unmatched_findings", 0) for r in bug)
    return {
        "bug cases scored": n,
        "bugs detected (recall)": pct(found, tot),
        "bugs fixed, proven only": pct(fixed_p, tot),
        "bugs fixed, with unproven": pct(fixed_a, tot),
        "extra findings on bug files": extra,
        "cases fully detected": pct(sum(1 for r in bug if r["detected"]), n),
        "strict pass@1 (proven fixes only)": pct(sum(1 for r in bug if r["oracle_proven_only"]), n),
        "lenient pass@1 (with unproven)": pct(sum(1 for r in bug if r["oracle_with_unproven"]), n),
        "false trust (proven but wrong)": sum(1 for r in bug if r["proven"] > 0 and not r["oracle_proven_only"]),
        "unproven correct": pct(sum(1 for r in unproven if r["oracle_with_unproven"]), len(unproven)),
        "clean controls": len(clean),
        "false alarms (clean code)": sum(1 for r in clean if r["confirmed"] > 0),
        "false-alarm findings (clean code)": sum(r["confirmed"] for r in clean),
        "broke clean code": sum(1 for r in clean if not r["oracle_with_unproven"]),
        "median wall s": med([r["wall_s"] for r in rows if r["status"] == "ok"]),
        "mean calls per case": round(statistics.mean([r["calls_main"] + r["calls_worker"] for r in rows if r["status"] == "ok"]), 1) if any(r["status"] == "ok" for r in rows) else None,
        "not scored (quota/error/timeout)": sum(1 for r in rows if r["status"] != "ok"),
    }


def print_table(rows):
    print(f"\n{'case':<26}{'category':<22}{'found':<7}{'ver':<5}{'prov':<6}{'strict':<8}{'lenient':<9}{'time':<8}{'calls'}")
    for r in rows:
        tick = lambda b: "-" if b is None else ("yes" if b else "no")
        if r["status"] != "ok":
            print(f"{r['case']:<26}{r['category']:<22}{r['status'].upper()}")
            continue
        print(f"{r['case']:<26}{r['category']:<22}{tick(r['detected']):<7}{r['verified']:<5}{r['proven']:<6}"
              f"{tick(r['oracle_proven_only']):<8}{tick(r['oracle_with_unproven']):<9}{str(r['wall_s']) + 's':<8}{r['calls_main'] + r['calls_worker']}")
    print()
    from collections import Counter
    for r in rows:
        if r["status"] == "ok" and r["category"] != "clean_control" and r["verified"] == 0:
            names = dict(Counter(c.split(":")[0] for c in r["failed_checks"]))
            print(f"  why {r['case']} was not fixed: reproduction ok={r['repro']}, guard ok={r['guard']}, failed checks={names}")
            for c in r["failed_checks"][:3]:
                print(f"      {c[:230]}")
    print()
    for k, v in summarize(rows).items():
        print(f"  {k:<36}{v}")


def run(args):
    if not Path(args.bin).exists():
        sys.exit(f"binary not found: {args.bin} (run `cargo build -p autoresolve-cli` first)")
    cases = load_cases(args.cases)
    if args.limit:
        cases = cases[: args.limit]
    RESULTS.mkdir(exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out = RESULTS / f"{args.label}-{stamp}.jsonl"
    args.evdir = RESULTS / f"{args.label}-{stamp}.events"
    args.evdir.mkdir(exist_ok=True)
    rows, used = [], 0
    for c in cases:
        if args.max_calls and used >= args.max_calls:
            print(f"stopping: {used} model calls used, budget is {args.max_calls}")
            break
        print(f"[bench] {c['id']} ...", flush=True)
        row = run_case(c, args)
        rows.append(row)
        used += row["calls_main"] + row["calls_worker"]
        with out.open("a") as f:
            f.write(json.dumps(row) + "\n")
        if row["status"] == "quota":
            print("stopping: the model quota is exhausted; resume tomorrow or switch models")
            break
    print_table(rows)
    print(f"\nresults: {out}")
    return 0


def compare(args):
    """One column per label. Result files with the same label are merged (a later run of a case replaces an
    earlier one), so a suite run in several sittings counts as one run. --cases limits every column to the
    same cases, which is what makes two configurations comparable."""
    files = [Path(f) for f in args.files] or sorted(RESULTS.glob("*.jsonl"))
    if not files:
        sys.exit("no result files")
    by_label = {}
    for f in files:
        for line in f.read_text().splitlines():
            if line.strip():
                r = json.loads(line)
                by_label.setdefault(r["label"], {})[r["case"]] = r
    only = set(args.cases) if args.cases else None
    cols = []
    for label, rows in by_label.items():
        picked = [r for c, r in rows.items() if not only or c in only]
        if picked:
            cols.append((f"{label} ({len(picked)} cases)", summarize(picked)))
    keys = list(cols[0][1].keys())
    print("| metric | " + " | ".join(n for n, _ in cols) + " |")
    print("|---|" + "---|" * len(cols))
    for k in keys:
        print(f"| {k} | " + " | ".join(str(s[k]) for _, s in cols) + " |")
    return 0


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("selfcheck")
    s.add_argument("--cases", nargs="*")
    s.set_defaults(fn=selfcheck)
    r = sub.add_parser("run")
    r.add_argument("--label", required=True, help="name of this configuration, e.g. gemini-split")
    r.add_argument("--bin", default=str(ROOT / "target" / "debug" / "autoresolve-cli"))
    r.add_argument("--cases", nargs="*", help="only these case ids")
    r.add_argument("--limit", type=int, help="run only the first N cases")
    r.add_argument("--max-calls", type=int, default=0, help="stop once this many model calls were used")
    r.add_argument("--timeout", type=int, default=1500, help="seconds allowed per case")
    r.set_defaults(fn=run)
    c = sub.add_parser("compare")
    c.add_argument("files", nargs="*")
    c.add_argument("--cases", nargs="*", help="compare only these case ids")
    c.set_defaults(fn=compare)
    a = p.parse_args()
    sys.exit(a.fn(a))


if __name__ == "__main__":
    main()
