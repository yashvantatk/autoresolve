def parse_int(text):
    """Return int(text), or None when text is not a valid integer string. Other errors must propagate."""
    try:
        return int(text)
    except:
        return None
