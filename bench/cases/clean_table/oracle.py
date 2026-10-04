import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from tablefmt import format_table
assert format_table([["a", "bb"], ["ccc", "d"]]) == "a" + " " * 4 + "bb\n" + "ccc" + " " * 2 + "d"
assert format_table([]) == ""
try:
    format_table([["a"], ["b", "c"]])
except ValueError:
    pass
else:
    raise AssertionError("ragged rows must raise")
