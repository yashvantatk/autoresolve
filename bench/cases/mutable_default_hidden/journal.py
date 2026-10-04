def record(event, log=[]):
    """Remember the event in the given log and return the event's length."""
    log.append(event)
    return len(event)
