import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from lrucache import LRUCache
c = LRUCache(2)
c.put("a", 1); c.put("b", 2); assert c.get("a") == 1
c.put("c", 3)
assert c.get("b") is None and c.get("a") == 1 and c.get("c") == 3
try:
    LRUCache(0)
except ValueError:
    pass
else:
    raise AssertionError("capacity 0 must raise")
