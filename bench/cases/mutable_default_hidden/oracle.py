import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from journal import record
assert record("ab") == 2
mine = []
assert record("x", mine) == 1 and mine == ["x"]
# the bug cannot be seen through calls, so the oracle (which agents never see) looks at the defaults
assert not any(isinstance(d, (list, dict, set)) for d in (record.__defaults__ or ())), "mutable default"
