import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
import math
from geometry import circle_area, clamp
assert abs(circle_area(1) - math.pi) < 1e-9
try:
    circle_area(-1)
except ValueError:
    pass
else:
    raise AssertionError("negative radius must raise")
assert clamp(5, 0, 3) == 3 and clamp(-1, 0, 3) == 0 and clamp(2, 0, 3) == 2
