# AutoResolve VS Code Extension

AI-powered Python code review and repair extension for VS Code that keeps **only the fixes it can prove in a sandbox**.

## Features

- **One-Click Run**: Click "Run AutoResolve" from the sidebar on any active Python file.
- **Live Agent Progress**: Real-time progress bar tracking the Reviewer, Skeptic, Tester, Guard Writer, Fixer, and Patch Gate agents by tailing `.autoresolve/events.jsonl`.
- **Native Red/Green Diffs**: Proven patches generated into `.autoresolve/plan.json` are rendered side-by-side using VS Code's native diff editor.
- **Accept or Reject**: One-click "Accept Fix" applies the search-and-replace edits directly to your workspace.
- **Fast AST Scan**: Free, instant tree-sitter scan for anti-patterns (mutable defaults, bare except, `== None`) via the AutoResolve MCP server without burning LLM quota.

## Development & Testing in Antigravity IDE

1. Open this folder in the Antigravity IDE:
   ```bash
   cd /home/yashvant/autoresolve/extension
   ```
2. Run `npm install`
3. Press `F5` to start the **Extension Development Host**.
4. In the Extension Development Host window, open a Python file and click the **AutoResolve** icon in the sidebar to test!
