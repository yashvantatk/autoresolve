import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from textutil import title_case, count_words
assert title_case("hello WORLD again") == "Hello World Again"
assert title_case("") == ""
assert count_words("a b  c") == 3 and count_words("") == 0
