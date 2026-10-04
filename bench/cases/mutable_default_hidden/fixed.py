def record(event, log=None):
    """Remember the event in the given log and return the event's length."""
    if log is None:
        log = []
    log.append(event)
    return len(event)
