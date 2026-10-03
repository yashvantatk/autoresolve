import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))

from demo5 import last_n

items = [1, 2, 3]
n = 1
assert last_n(items, n) == [3]
