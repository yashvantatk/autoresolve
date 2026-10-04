import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from stats import average
assert average([1, 2, 3]) == 2
assert average([2.5]) == 2.5
assert average([]) == 0.0
