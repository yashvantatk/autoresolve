import shlex
import subprocess


def run_cmd(cmd):
    """Run a command line such as 'echo hello' and return its output as text."""
    return subprocess.run(shlex.split(cmd), capture_output=True, text=True).stdout
