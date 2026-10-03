import os

def run_cmd(cmd, history=[]):
    history.append(cmd)
    return os.popen(cmd).read()
