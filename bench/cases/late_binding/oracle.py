import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from mult import make_multipliers
assert [f(10) for f in make_multipliers(3)] == [0, 10, 20]
assert make_multipliers(0) == []
