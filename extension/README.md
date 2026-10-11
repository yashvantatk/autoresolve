# AutoResolve VS Code Extension

**AI-Assisted & Deterministic Code Review and Repair for Python**

AutoResolve finds bugs in Python code and **only proposes fixes that are proven to work**. It features an automated hybrid execution engine that intelligently switches between an instant, zero-config deterministic AST patcher and a verified multi-agent AI pipeline.

---

## ⚡ Quick Start: 3 Execution Modes

AutoResolve operates in three distinct modes depending on your setup. You don't need to configure environment variables or build Rust binaries.

```
+-----------------------------------------------------------------------------------+
|                              AutoResolve Hybrid Router                             |
+-----------------------------------------------------------------------------------+
       |                                                               |
 [No API Key Found]                                       [Valid API Key / Ollama]
       v                                                               v
+----------------------------+                           +--------------------------+
|     DETERMINISTIC MODE     |                           |       AI COMMITTEES      |
|  - Zero configuration      |                           |  - Cloud AI (Gemini)     |
|  - Instant Tree-sitter AST |                           |  - Local AI (Ollama)     |
|  - Ruff lint & security    |                           |  - Test reproducer loop  |
|  - No Docker / No LLM      |                           |  - Docker verification  |
+----------------------------+                           +--------------------------+
```

---

### 1. Deterministic Mode (Zero-Config & Instant)
- **Requirements:** None! No API key, no Docker, no external services.
- **How to use:** Just install the extension, open any `.py` file, and click **▶ Run AutoResolve** in the sidebar or status bar.
- **What it fixes automatically:**
  - **Security:** Replaces dangerous `subprocess.run(..., shell=True)` with `shell=False`.
  - **PEP 8 / Correctness:** Fixes invalid `None` equality checks (`x == None` $\to$ `x is None`, `x != None` $\to$ `x is not None`).
  - **Robustness:** Replaces broad `except:` clauses with explicit `except Exception:`.
  - **Formatting & Linting:** Applies automated Ruff linting and formatting fixes.

---

### 2. Cloud AI Mode (Multi-Agent Verification with Gemini)
- **Requirements:** A Google Gemini API key ([Get one free from Google AI Studio](https://aistudio.google.com/app/apikey)) and [Docker Desktop](https://docs.docker.com/get-docker/).
- **How it works:** Spawns a 6-agent committee (Reviewer, Skeptic, Tester, Fixer, Behavior Guard, Gatekeeper). Fixes are **only** accepted if the Tester writes an isolated test reproducer that fails before the patch and passes after it.

#### 🔑 How to Set Your Gemini API Key:
1. Open the VS Code **Command Palette**:
   - **Windows / Linux:** `Ctrl` + `Shift` + `P`
   - **macOS:** `Cmd` + `Shift` + `P`
2. Type **`AutoResolve: Set Gemini API Key`** and press `Enter`.
3. Paste your Gemini API key (`AIzaSy...`) into the input box and press `Enter`.
4. Your API key is now securely encrypted in your operating system's keychain using VS Code's native `SecretStorage` API.

#### 🔄 How to Clear Your Key (Revert to Deterministic Mode):
1. Press `Ctrl` + `Shift` + `P` (or `Cmd` + `Shift` + `P`).
2. Run **`AutoResolve: Set Gemini API Key`**.
3. Leave the input box completely blank and press `Enter`.
4. The key will be deleted from secure storage, and AutoResolve will automatically revert to Deterministic Mode.

---

### 3. Local AI Mode (Private & Offline with Ollama)
- **Requirements:** A running [Ollama](https://ollama.com/) instance and Docker.
- **How to enable:**
  1. Open VS Code Settings (`Ctrl` + `,` or `Cmd` + `,`).
  2. Search for `autoresolve.provider`.
  3. Select **`ollama`** from the dropdown menu.
  4. AutoResolve will route multi-agent reasoning through your local models with zero cloud data egress.

---

## 🛠️ Usage & Workflow

### 1. Run AutoResolve on an Active File
- Open any Python (`.py`) file in your editor.
- Click **▶ Run AutoResolve** in the AutoResolve sidebar panel, or click the **`$(shield) AutoResolve`** button in the status bar at the bottom right.
- Alternatively, press `Ctrl` + `Shift` + `P` and run **`AutoResolve: Run AutoResolve on Active File`**.

### 2. Monitor Real-Time Progress
- Expand the **AutoResolve Control** view in the sidebar.
- Watch live agent telemetry in the **Live Agent Feed** as the Reviewer, Tester, and Fixer analyze your code.

### 3. Review Native Red/Green Diffs
- When a verified fix is proposed, click **View Diff (Red/Green)** on the fix card.
- AutoResolve opens VS Code's native side-by-side diff viewer (`vscode.diff`) highlighting the exact lines modified.

### 4. Accept or Reject Fixes
- Click **Accept Fix** to apply the verified patch directly to your file.
- Click **Reject Fix** to discard the proposal.

### 5. Run Fast AST Scans
- Click **Run AST Scan** in the sidebar to scan your workspace for known anti-patterns without invoking any LLMs or verification containers.

---

## ⌨️ Command Palette Reference

All extension commands can be triggered via `Ctrl` + `Shift` + `P` (`Cmd` + `Shift` + `P` on macOS):

| Command Title | ID | Description |
| :--- | :--- | :--- |
| **`AutoResolve: Set Gemini API Key`** | `autoresolve.setApiKey` | Securely store or clear your Gemini API key in SecretStorage. |
| **`AutoResolve: Run AutoResolve on Active File`** | `autoresolve.runFix` | Run the AutoResolve repair engine on the currently focused Python file. |
| **`AutoResolve: Run AST Scan`** | `autoresolve.runScan` | Run a fast static AST anti-pattern scan without LLMs. |
| **`AutoResolve: Review Proven Fixes (Diff)`** | `autoresolve.viewPlanDiffs` | View the red/green diff for the latest generated fix plan. |
| **`AutoResolve: Review Active File (MCP)`** | `autoresolve.reviewActiveFile` | Generate a comprehensive code review markdown report via MCP. |

---

## ⚙️ Extension Settings

Accessible via `Ctrl` + `,` (or `Cmd` + `,`) under **Extensions** $\to$ **AutoResolve**:

| Setting | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `autoresolve.provider` | `enum` | `"gemini"` | LLM provider to use: `"gemini"` or `"ollama"`. |
| `autoresolve.cliPath` | `string` | `""` | Optional override path to the `autoresolve-cli` binary. When blank, the auto-downloaded native binary is used. |
| `autoresolve.pythonCommand` | `string` | `"python3"` | Python interpreter used for running local test suites. |

---

## 🔒 Security & Sandboxing

- **Encrypted Secret Storage:** API keys are never stored in plain text files or environment variables; they are encrypted via VS Code's OS-level `SecretStorage` keychain.
- **Isolated Containers:** Cloud AI and Local AI test reproducer runs occur inside isolated, network-disabled Docker containers with read-only root filesystems.
- **Protected Paths:** Sensitive files (such as `.git/config`, `policy.toml`, and regression test suites) are strictly write-protected from automated modifications.
- **Zero-Cloud Deterministic Mode:** Deterministic fixes execute 100% locally in-process with zero network requests and zero LLM calls.