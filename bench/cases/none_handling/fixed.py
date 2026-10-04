USERS = {"ann": {"email": "ann@example.com"}, "bob": {"email": "bob@example.com"}}


def find_user(name):
    return USERS.get(name)


def user_email(name):
    """Return the user's email address, or None when there is no such user."""
    user = find_user(name)
    return user["email"] if user else None
