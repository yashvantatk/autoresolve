import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from parsing import parse_int
assert parse_int("12") == 12
assert parse_int("x") is None
try:
    parse_int(None)
except TypeError:
    pass
else:
    raise AssertionError("TypeError must propagate")
