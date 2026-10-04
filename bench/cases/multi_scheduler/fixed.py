def pick_slot(slots, hour):
    """Return the slot for the given hour, where hour 0 is the first slot."""
    return slots[hour]


def average_wait(waits):
    """Return the mean waiting time, or 0.0 when nobody waited."""
    if not waits:
        return 0.0
    return sum(waits) / len(waits)


def normalize_name(name):
    """Return the name without surrounding spaces, in lowercase."""
    return name.strip().lower()
