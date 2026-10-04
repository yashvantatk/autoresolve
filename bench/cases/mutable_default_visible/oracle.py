import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from tagging import add_tag
assert add_tag("a") == ["a"]
assert add_tag("b") == ["b"]
mine = ["x"]
assert add_tag("y", mine) is mine and mine == ["x", "y"]
