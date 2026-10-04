def make_multipliers(n):
    """Return n functions; the i-th one multiplies its argument by i."""
    return [lambda x: x * i for i in range(n)]
