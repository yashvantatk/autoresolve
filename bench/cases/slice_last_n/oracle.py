import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from listutil import last_n
assert last_n([1, 2, 3], 2) == [2, 3]
assert last_n([1, 2, 3], 0) == []
assert last_n([1, 2], 5) == [1, 2]
assert last_n([], 1) == []
