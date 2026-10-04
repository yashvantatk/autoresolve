import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
import sqlite3
from repo import find_user
conn = sqlite3.connect(":memory:")
conn.execute("CREATE TABLE users (id INTEGER, name TEXT)")
conn.executemany("INSERT INTO users VALUES (?, ?)", [(1, "ann"), (2, "bob"), (3, "o'neil")])
assert find_user(conn, "ann") == [(1, "ann")]
assert find_user(conn, "o'neil") == [(3, "o'neil")]
assert find_user(conn, "x' OR '1'='1") == []
