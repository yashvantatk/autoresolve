import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
import warnings
warnings.simplefilter("ignore")
from status import is_ok
assert is_ok("".join(["o", "k"])) is True
assert is_ok("no") is False
