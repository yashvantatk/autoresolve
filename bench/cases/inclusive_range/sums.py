def sum_to(n):
    """Return 1 + 2 + ... + n (n included)."""
    total = 0
    for i in range(n):
        total += i
    return total
