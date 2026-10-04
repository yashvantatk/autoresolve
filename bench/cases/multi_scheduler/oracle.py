import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
RESULTS = []
def check(name, fn):
    try:
        fn()
        RESULTS.append((name, True))
    except Exception:
        RESULTS.append((name, False))

from scheduler import pick_slot, average_wait, normalize_name
def _slot(): assert pick_slot(["a", "b", "c"], 0) == "a" and pick_slot(["a", "b", "c"], 2) == "c"
def _avg(): assert average_wait([2, 4]) == 3 and average_wait([]) == 0.0
def _norm(): assert normalize_name("  Ann ") == "ann"
check("pick_slot", _slot)
check("average_wait", _avg)
check("normalize_name", _norm)

for _n, _ok in RESULTS:
    print(("OK " if _ok else "FAIL ") + _n)
sys.exit(0 if all(ok for _, ok in RESULTS) else 1)
