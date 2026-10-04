def remove_negatives(d):
    """Remove every entry with a negative value from the dict d, in place, and return d."""
    for key in d:
        if d[key] < 0:
            del d[key]
    return d
