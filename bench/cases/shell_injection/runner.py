import os


def run_cmd(cmd):
    """Run a command line such as 'echo hello' and return its output as text."""
    return os.popen(cmd).read()
