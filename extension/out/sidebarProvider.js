"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.SidebarProvider = void 0;
const vscode = __importStar(require("vscode"));
class SidebarProvider {
    extensionUri;
    onRunFix;
    onRunScan;
    onViewDiff;
    view;
    constructor(extensionUri, onRunFix, onRunScan, onViewDiff) {
        this.extensionUri = extensionUri;
        this.onRunFix = onRunFix;
        this.onRunScan = onRunScan;
        this.onViewDiff = onViewDiff;
    }
    resolveWebviewView(webviewView, _context, _token) {
        this.view = webviewView;
        webviewView.webview.options = {
            enableScripts: true,
            localResourceRoots: [this.extensionUri],
        };
        webviewView.webview.html = this.getHtmlForWebview();
        webviewView.webview.onDidReceiveMessage((data) => {
            switch (data.type) {
                case "runFix": {
                    const editor = vscode.window.activeTextEditor;
                    if (editor && editor.document.languageId === "python") {
                        const rel = vscode.workspace.asRelativePath(editor.document.uri);
                        this.onRunFix(rel);
                    }
                    else {
                        vscode.window.showWarningMessage("Please open a Python file first.");
                    }
                    break;
                }
                case "runScan": {
                    this.onRunScan();
                    break;
                }
                case "viewDiff": {
                    this.onViewDiff(data.item, data.index);
                    break;
                }
            }
        });
        this.updateActiveFile();
    }
    updateActiveFile() {
        const editor = vscode.window.activeTextEditor;
        const file = editor && editor.document.languageId === "python"
            ? vscode.workspace.asRelativePath(editor.document.uri)
            : null;
        this.postMessage({ type: "activeFile", file });
    }
    updateProgress(p) {
        this.postMessage({ type: "progress", progress: p });
    }
    addEvent(e) {
        this.postMessage({ type: "event", event: e });
    }
    setPlan(plan) {
        this.postMessage({ type: "plan", plan });
    }
    setFindings(findings) {
        this.postMessage({ type: "findings", findings });
    }
    postMessage(message) {
        this.view?.webview.postMessage(message);
    }
    getHtmlForWebview() {
        return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>AutoResolve</title>
  <style>
    body {
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      color: var(--vscode-foreground);
      background-color: var(--vscode-sideBar-background);
      padding: 12px;
      margin: 0;
    }
    h2, h3 {
      font-weight: 600;
      margin-top: 0;
      margin-bottom: 8px;
    }
    .file-bar {
      background: var(--vscode-editor-background);
      border: 1px solid var(--vscode-panel-border);
      border-radius: 4px;
      padding: 8px;
      margin-bottom: 12px;
      font-family: var(--vscode-editor-font-family);
      font-size: 0.9em;
      word-break: break-all;
    }
    .btn {
      display: block;
      width: 100%;
      background: var(--vscode-button-background);
      color: var(--vscode-button-foreground);
      border: none;
      border-radius: 4px;
      padding: 9px;
      font-size: 13px;
      font-weight: 600;
      cursor: pointer;
      text-align: center;
      margin-bottom: 8px;
    }
    .btn:hover {
      background: var(--vscode-button-hoverBackground);
    }
    .btn.secondary {
      background: var(--vscode-button-secondaryBackground);
      color: var(--vscode-button-secondaryForeground);
    }
    .btn.secondary:hover {
      background: var(--vscode-button-secondaryHoverBackground);
    }
    .progress-box {
      background: var(--vscode-editor-background);
      border: 1px solid var(--vscode-panel-border);
      border-radius: 6px;
      padding: 10px;
      margin-top: 12px;
      display: none;
    }
    .progress-bar-bg {
      background: var(--vscode-progressBar-background, #333);
      border-radius: 4px;
      height: 8px;
      width: 100%;
      overflow: hidden;
      margin-top: 6px;
      margin-bottom: 8px;
    }
    .progress-bar-fill {
      background: var(--vscode-button-background);
      height: 100%;
      width: 0%;
      transition: width 0.3s ease;
    }
    .role-badge {
      display: inline-block;
      font-size: 10px;
      font-weight: 700;
      text-transform: uppercase;
      padding: 2px 6px;
      border-radius: 3px;
      background: var(--vscode-badge-background);
      color: var(--vscode-badge-foreground);
    }
    .status-text {
      font-size: 0.85em;
      color: var(--vscode-descriptionForeground);
      margin-top: 4px;
    }
    .card {
      background: var(--vscode-editor-background);
      border: 1px solid var(--vscode-panel-border);
      border-radius: 4px;
      padding: 8px 10px;
      margin-top: 8px;
    }
    .card.proven {
      border-left: 3px solid #4ec9b0;
    }
    .card.unproven {
      border-left: 3px solid #cca700;
    }
    .badge-proven {
      background: #1e4620;
      color: #73c991;
      padding: 2px 6px;
      border-radius: 3px;
      font-size: 10px;
      font-weight: bold;
    }
    .card-title {
      font-weight: 600;
      font-size: 0.95em;
      margin-bottom: 4px;
    }
    .card-summary {
      font-size: 0.85em;
      color: var(--vscode-descriptionForeground);
      margin-bottom: 6px;
    }
    .log-box {
      max-height: 140px;
      overflow-y: auto;
      font-family: var(--vscode-editor-font-family);
      font-size: 11px;
      background: var(--vscode-editor-background);
      border: 1px solid var(--vscode-panel-border);
      border-radius: 4px;
      padding: 6px;
      margin-top: 10px;
    }
    .log-entry {
      margin-bottom: 3px;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }
  </style>
</head>
<body>
  <h3>AutoResolve</h3>
  <div id="fileDisplay" class="file-bar">No active Python file</div>
  <button id="btnRunFix" class="btn">▶ Run AutoResolve</button>
  <button id="btnRunScan" class="btn secondary">🔍 Fast AST Scan (Free)</button>

  <div id="progressBox" class="progress-box">
    <div style="display: flex; justify-content: space-between; align-items: center;">
      <span id="roleBadge" class="role-badge">AGENT</span>
      <span id="percentText" style="font-size: 11px; font-weight: 600;">0%</span>
    </div>
    <div class="progress-bar-bg">
      <div id="progressBar" class="progress-bar-fill"></div>
    </div>
    <div id="statusDetail" class="status-text">Starting pipeline...</div>
  </div>

  <div id="planSection" style="margin-top: 14px; display: none;">
    <h4 style="margin: 0 0 6px 0;">Proven Fixes</h4>
    <div id="planItems"></div>
  </div>

  <div id="findingsSection" style="margin-top: 14px; display: none;">
    <h4 style="margin: 0 0 6px 0;">AST Findings</h4>
    <div id="findingsList"></div>
  </div>

  <div id="logSection" style="margin-top: 14px;">
    <h4 style="margin: 0 0 4px 0; font-size: 12px; color: var(--vscode-descriptionForeground);">Live Agent Feed</h4>
    <div id="logBox" class="log-box"></div>
  </div>

  <script>
    const vscode = acquireVsCodeApi();
    const btnRunFix = document.getElementById("btnRunFix");
    const btnRunScan = document.getElementById("btnRunScan");
    const fileDisplay = document.getElementById("fileDisplay");
    const progressBox = document.getElementById("progressBox");
    const roleBadge = document.getElementById("roleBadge");
    const percentText = document.getElementById("percentText");
    const progressBar = document.getElementById("progressBar");
    const statusDetail = document.getElementById("statusDetail");
    const logBox = document.getElementById("logBox");
    const planSection = document.getElementById("planSection");
    const planItems = document.getElementById("planItems");
    const findingsSection = document.getElementById("findingsSection");
    const findingsList = document.getElementById("findingsList");

    let currentPlan = null;

    btnRunFix.addEventListener("click", () => {
      vscode.postMessage({ type: "runFix" });
    });

    btnRunScan.addEventListener("click", () => {
      vscode.postMessage({ type: "runScan" });
    });

    function updateProgressBar(percent, detail, role) {
      progressBox.style.display = "block";
      if (role) {
        roleBadge.textContent = role.toUpperCase();
      }
      percentText.textContent = percent + "%";
      progressBar.style.width = percent + "%";
      statusDetail.textContent = detail;
    }

    window.addEventListener("message", (event) => {
      const msg = event.data;
      switch (msg.type) {
        case "activeFile":
          fileDisplay.textContent = msg.file ? msg.file : "No active Python file";
          btnRunFix.disabled = !msg.file;
          break;

        case "progress":
          updateProgressBar(msg.progress.percent, msg.progress.detail, msg.progress.currentRole);
          break;

        case "event":
          const item = document.createElement("div");
          item.className = "log-entry";
          item.textContent = "[" + msg.event.role + "] " + msg.event.kind;
          logBox.appendChild(item);
          logBox.scrollTop = logBox.scrollHeight;

          if (msg.event && msg.event.kind === "run_end") {
            updateProgressBar(100, "Complete", "DONE");
          }
          break;

        case "plan":
          currentPlan = msg.plan;
          planItems.innerHTML = "";
          updateProgressBar(100, "Complete", "DONE");
          if (msg.plan && msg.plan.items && msg.plan.items.length > 0) {
            planSection.style.display = "block";
            msg.plan.items.forEach((it, idx) => {
              const card = document.createElement("div");
              card.className = "card " + (it.proven ? "proven" : "unproven");
              card.innerHTML = \`
                <div style="display: flex; justify-content: space-between; align-items: center;">
                  <div class="card-title">\${it.title}</div>
                  <span class="badge-proven">\${it.proven ? "PROVEN" : "UNPROVEN"}</span>
                </div>
                <div class="card-summary">\${it.summary}</div>
                <button class="btn secondary" style="padding: 4px 8px; font-size: 11px; margin-top: 4px;" onclick="viewDiff(\${idx})">
                  View Diff (Red/Green)
                </button>
              \`;
              planItems.appendChild(card);
            });
          }
          break;

        case "findings":
          findingsList.innerHTML = "";
          if (msg.findings && msg.findings.length > 0) {
            findingsSection.style.display = "block";
            msg.findings.forEach(f => {
              const el = document.createElement("div");
              el.className = "card";
              el.innerHTML = \`
                <div class="card-title">\${f.rule}: Line \${f.line}</div>
                <div class="card-summary">\${f.message}</div>
              \`;
              findingsList.appendChild(el);
            });
          } else {
            findingsSection.style.display = "none";
          }
          break;
      }
    });

    function viewDiff(idx) {
      if (currentPlan && currentPlan.items[idx]) {
        vscode.postMessage({ type: "viewDiff", item: currentPlan.items[idx], index: idx });
      }
    }
  </script>
</body>
</html>`;
    }
}
exports.SidebarProvider = SidebarProvider;
//# sourceMappingURL=sidebarProvider.js.map