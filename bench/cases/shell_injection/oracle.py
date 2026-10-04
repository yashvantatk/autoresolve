import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
import tempfile
from runner import run_cmd
assert run_cmd("echo hello") == "hello\n"
with tempfile.TemporaryDirectory() as d:
    marker = os.path.join(d, "pwned")
    try:
        run_cmd("echo hi; touch " + marker)
    except Exception:
        pass
    assert not os.path.exists(marker), "the injected command ran"
