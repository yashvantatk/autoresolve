import time


def retry(func, attempts=3, delay=0.0):
    """Call func() until it succeeds; after the last failed attempt re-raise its error."""
    if attempts < 1:
        raise ValueError("attempts must be at least 1")
    last_error = None
    for attempt in range(attempts):
        try:
            return func()
        except Exception as error:
            last_error = error
            if attempt < attempts - 1 and delay:
                time.sleep(delay)
    raise last_error
