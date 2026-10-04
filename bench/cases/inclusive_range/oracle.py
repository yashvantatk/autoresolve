import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from sums import sum_to
assert sum_to(4) == 10
assert sum_to(1) == 1
assert sum_to(0) == 0
