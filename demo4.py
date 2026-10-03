def add_tag(tag, tags=None):
    if tags is None:
        tags = []
    tags.append(tag)
    return tags
