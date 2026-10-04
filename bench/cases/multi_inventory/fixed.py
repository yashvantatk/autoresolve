def total_price(items):
    """Return the sum of price * quantity over (name, price, quantity) tuples."""
    total = 0
    for name, price, quantity in items:
        total += price * quantity
    return total


def find_item(items, name):
    """Return the (name, price, quantity) tuple called name, or None when there is none."""
    for item in items:
        if item[0] == name:
            return item
    return None


def top_n(values, n):
    """Return the n largest values, largest first."""
    return sorted(values, reverse=True)[:n]
