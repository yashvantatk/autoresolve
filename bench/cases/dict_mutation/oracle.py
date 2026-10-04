import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from cleanup import remove_negatives
data = {"a": 1, "b": -1, "c": 2, "d": -3}
result = remove_negatives(data)
assert result is data and data == {"a": 1, "c": 2}
