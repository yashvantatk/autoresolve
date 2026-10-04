def last_n(items, n):
    """Return the last n items of the list (an empty list when n is 0)."""
    return items[max(len(items) - n, 0):] if n > 0 else []
