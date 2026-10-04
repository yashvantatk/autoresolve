import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from retrying import retry
calls = []
def flaky():
    calls.append(1)
    if len(calls) < 3:
        raise RuntimeError("not yet")
    return "ok"
assert retry(flaky, attempts=3) == "ok" and len(calls) == 3
def broken():
    raise KeyError("always")
try:
    retry(broken, attempts=2)
except KeyError:
    pass
else:
    raise AssertionError("the last error must be re-raised")
try:
    retry(broken, attempts=0)
except ValueError:
    pass
else:
    raise AssertionError("attempts=0 must raise ValueError")
