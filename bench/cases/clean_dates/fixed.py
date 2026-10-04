def is_leap_year(year):
    """Return True for leap years in the Gregorian calendar."""
    return year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)


def days_in_month(year, month):
    """Return the number of days in the given month (1-12)."""
    if not 1 <= month <= 12:
        raise ValueError("month must be between 1 and 12")
    if month == 2:
        return 29 if is_leap_year(year) else 28
    return 30 if month in (4, 6, 9, 11) else 31
