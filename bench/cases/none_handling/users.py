USERS = {"ann": {"email": "ann@example.com"}, "bob": {"email": "bob@example.com"}}


def find_user(name):
    return USERS.get(name)


def user_email(name):
    """Return the user's email address, or None when there is no such user."""
    return find_user(name)["email"]
