import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))

from buggy import add_all, last_item
from sample import Cart

assert add_all(Cart(), [1]) == [1]
assert add_all(Cart(), [2]) == [2]   # fails if the default list leaks state between calls
assert last_item([]) is None
