import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))

from demo4 import add_tag

tags1 = add_tag('tag1')
tags2 = add_tag('tag2')

assert tags1 == ['tag1']
assert tags2 == ['tag2'], f"Expected ['tag2'], but got {tags2}"
