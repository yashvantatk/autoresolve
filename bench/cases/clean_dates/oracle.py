import os, sys
sys.path.insert(0, os.environ.get('BENCH_DIR', '.'))
from dates import is_leap_year, days_in_month
assert is_leap_year(2000) and not is_leap_year(1900) and is_leap_year(2024) and not is_leap_year(2023)
assert days_in_month(2024, 2) == 29 and days_in_month(2023, 2) == 28
assert days_in_month(2023, 4) == 30 and days_in_month(2023, 1) == 31
try:
    days_in_month(2023, 13)
except ValueError:
    pass
else:
    raise AssertionError("month 13 must raise")
