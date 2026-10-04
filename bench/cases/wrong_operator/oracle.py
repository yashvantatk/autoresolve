import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from age import is_adult
assert is_adult(18) is True
assert is_adult(17) is False
assert is_adult(40) is True
