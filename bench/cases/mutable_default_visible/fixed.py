def add_tag(tag, tags=None):
    """Return a list holding tag: appended to tags when given, a fresh list otherwise."""
    if tags is None:
        tags = []
    tags.append(tag)
    return tags
