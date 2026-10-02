import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))

from buggy import average

# If average is called with an empty list, it should ideally handle it 
# gracefully or raise a meaningful exception rather than ZeroDivisionError.
# Given the bug description mentions a ZeroDivisionError, we check for it.
try:
    average([])
except ZeroDivisionError:
    # This confirms the bug
    raise AssertionError("average() raised ZeroDivisionError on empty list")

