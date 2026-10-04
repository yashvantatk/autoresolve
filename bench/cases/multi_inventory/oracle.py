import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
RESULTS = []
def check(name, fn):
    try:
        fn()
        RESULTS.append((name, True))
    except Exception:
        RESULTS.append((name, False))

from inventory import total_price, find_item, top_n
def _total(): assert total_price([("a", 2, 3), ("b", 5, 4)]) == 26
def _find():
    items = [("a", 1, 1), ("b", 2, 2)]
    assert find_item(items, "b") == ("b", 2, 2) and find_item(items, "zzz") is None
def _top(): assert top_n([3, 1, 4, 1, 5], 2) == [5, 4]
check("total_price", _total)
check("find_item", _find)
check("top_n", _top)

for _n, _ok in RESULTS:
    print(("OK " if _ok else "FAIL ") + _n)
sys.exit(0 if all(ok for _, ok in RESULTS) else 1)
