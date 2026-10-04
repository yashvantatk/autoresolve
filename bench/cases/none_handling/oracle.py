import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from users import user_email
assert user_email("ann") == "ann@example.com"
assert user_email("bob") == "bob@example.com"
assert user_email("zed") is None
