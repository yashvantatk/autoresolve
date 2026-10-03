# Sandbox image for AutoResolve: the test runner plus the checkers used to verify patches.
# The sandbox runs with --network none, so everything must be installed here at build time.
FROM python:3.12-slim
RUN pip install --no-cache-dir pytest ruff bandit mypy
