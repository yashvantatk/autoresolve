# AutoResolve VS Code Extension

This is the UI frontend for the AutoResolve multi-agent pipeline. 

**⚠️ Important Setup Required**
This extension does not bundle the AI engine or API keys. It requires the Rust `autoresolve-cli` backend running locally on your machine.

**1. Build the Backend:**
Clone the [AutoResolve repository](https://github.com/yashvantatk/autoresolve), install Rust, and compile the CLI:
`cargo build`

**2. Bring Your Own Key (BYOK):**
Set your provider and API key in your terminal environment before launching VS Code:
`export AUTORESOLVE_PROVIDER=gemini`
`export GEMINI_API_KEY=your_api_key_here`

**3. Connect the CLI:**
Ensure the compiled `autoresolve-cli` binary is available in your system's PATH (e.g., symlinked to `/usr/local/bin`).