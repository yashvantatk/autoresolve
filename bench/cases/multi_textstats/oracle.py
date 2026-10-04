import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
RESULTS = []
def check(name, fn):
    try:
        fn()
        RESULTS.append((name, True))
    except Exception:
        RESULTS.append((name, False))

from textstats import word_freq, longest_word, is_palindrome
def _freq(): assert word_freq("a b A") == {"a": 2, "b": 1}
def _long(): assert longest_word("a ccc bb") == "ccc" and longest_word("") == ""
def _pal(): assert is_palindrome("Never odd or even") is True and is_palindrome("abc") is False
check("word_freq", _freq)
check("longest_word", _long)
check("is_palindrome", _pal)

for _n, _ok in RESULTS:
    print(("OK " if _ok else "FAIL ") + _n)
sys.exit(0 if all(ok for _, ok in RESULTS) else 1)
