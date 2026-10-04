import sqlite3


def find_user(conn, name):
    """Return the (id, name) rows of users whose name equals the given name."""
    return conn.execute("SELECT id, name FROM users WHERE name = ?", (name,)).fetchall()
