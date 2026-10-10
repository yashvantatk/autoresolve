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
exports.activate = activate;
exports.deactivate = deactivate;
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
const vscode = __importStar(require("vscode"));
const diffViewer_1 = require("./diffViewer");
const mcpClient_1 = require("./mcpClient");
const pipelineRunner_1 = require("./pipelineRunner");
const sidebarProvider_1 = require("./sidebarProvider");
let mcpClient = null;
let pipelineRunner = null;
let diffViewer = null;
let sidebarProvider = null;
let statusBarItem;
function resolveBinaryPath(workspaceRoot) {
    const config = vscode.workspace.getConfiguration("autoresolve");
    const configured = config.get("cliPath");
    if (configured && fs.existsSync(configured)) {
        return configured;
    }
    const debugBin = path.join(workspaceRoot, "target", "debug", "autoresolve-cli");
    if (fs.existsSync(debugBin)) {
        return debugBin;
    }
    const releaseBin = path.join(workspaceRoot, "target", "release", "autoresolve-cli");
    if (fs.existsSync(releaseBin)) {
        return releaseBin;
    }
    // Parent dir check if extension is in subfolder
    const parentDebug = path.join(workspaceRoot, "..", "target", "debug", "autoresolve-cli");
    if (fs.existsSync(parentDebug)) {
        return path.resolve(parentDebug);
    }
    return "autoresolve-cli";
}
function activate(context) {
    const workspaceFolders = vscode.workspace.workspaceFolders;
    const workspaceRoot = workspaceFolders && workspaceFolders.length > 0
        ? workspaceFolders[0].uri.fsPath
        : process.cwd();
    const binaryPath = resolveBinaryPath(workspaceRoot);
    // 1. Content provider for diff previews
    const contentProvider = new diffViewer_1.AutoResolveContentProvider();
    context.subscriptions.push(vscode.workspace.registerTextDocumentContentProvider(diffViewer_1.AutoResolveContentProvider.scheme, contentProvider));
    diffViewer = new diffViewer_1.DiffViewer(workspaceRoot, contentProvider);
    // 2. MCP client
    mcpClient = new mcpClient_1.McpClient(binaryPath, workspaceRoot);
    // 3. Pipeline runner
    pipelineRunner = new pipelineRunner_1.PipelineRunner(binaryPath, workspaceRoot);
    // 4. Sidebar Webview Provider
    sidebarProvider = new sidebarProvider_1.SidebarProvider(context.extensionUri, async (file) => {
        await runFixOnFile(file);
    }, async () => {
        await runScan();
    }, async (item, idx) => {
        await diffViewer?.showFixDiff(item, idx);
    });
    context.subscriptions.push(vscode.window.registerWebviewViewProvider("autoresolve.sidebar", sidebarProvider));
    // 5. Status bar item
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    statusBarItem.command = "autoresolve.runFix";
    statusBarItem.text = "$(shield) AutoResolve";
    statusBarItem.tooltip = "Run AutoResolve on Active Python File";
    statusBarItem.show();
    context.subscriptions.push(statusBarItem);
    // 6. Commands
    context.subscriptions.push(vscode.commands.registerCommand("autoresolve.runFix", async () => {
        const editor = vscode.window.activeTextEditor;
        if (!editor || editor.document.languageId !== "python") {
            vscode.window.showWarningMessage("Please open a Python file to run AutoResolve.");
            return;
        }
        const relPath = vscode.workspace.asRelativePath(editor.document.uri);
        await runFixOnFile(relPath);
    }));
    context.subscriptions.push(vscode.commands.registerCommand("autoresolve.runScan", async () => {
        await runScan();
    }));
    context.subscriptions.push(vscode.commands.registerCommand("autoresolve.reviewActiveFile", async () => {
        const editor = vscode.window.activeTextEditor;
        if (!editor || editor.document.languageId !== "python") {
            vscode.window.showWarningMessage("Please open a Python file to review.");
            return;
        }
        const relPath = vscode.workspace.asRelativePath(editor.document.uri);
        vscode.window.withProgress({
            location: vscode.ProgressLocation.Notification,
            title: `AutoResolve: Reviewing ${relPath}...`,
            cancellable: false,
        }, async () => {
            try {
                const report = await mcpClient?.review(relPath);
                const doc = await vscode.workspace.openTextDocument({
                    content: report || "No review findings.",
                    language: "markdown",
                });
                await vscode.window.showTextDocument(doc);
            }
            catch (e) {
                vscode.window.showErrorMessage(`AutoResolve review failed: ${e.message}`);
            }
        });
    }));
    context.subscriptions.push(vscode.commands.registerCommand("autoresolve.viewPlanDiffs", async () => {
        const planPath = path.join(workspaceRoot, ".autoresolve", "plan.json");
        if (!fs.existsSync(planPath)) {
            vscode.window.showInformationMessage("No plan.json found. Run AutoResolve first.");
            return;
        }
        try {
            const plan = JSON.parse(fs.readFileSync(planPath, "utf8"));
            sidebarProvider?.setPlan(plan);
            if (plan.items && plan.items.length > 0) {
                await diffViewer?.showFixDiff(plan.items[0], 0);
            }
            else {
                vscode.window.showInformationMessage("Plan contains 0 items.");
            }
        }
        catch (e) {
            vscode.window.showErrorMessage(`Failed to read plan: ${e.message}`);
        }
    }));
    // Listen to active editor switches
    context.subscriptions.push(vscode.window.onDidChangeActiveTextEditor(() => {
        sidebarProvider?.updateActiveFile();
    }));
}
async function runFixOnFile(relPath) {
    if (!pipelineRunner) {
        return;
    }
    if (pipelineRunner.running) {
        vscode.window.showWarningMessage("AutoResolve is already running.");
        return;
    }
    statusBarItem.text = "$(sync~spin) AutoResolve Running...";
    try {
        const plan = await pipelineRunner.runFix(relPath, (progress) => {
            sidebarProvider?.updateProgress(progress);
        }, (ev) => {
            sidebarProvider?.addEvent(ev);
        });
        statusBarItem.text = "$(shield) AutoResolve";
        if (plan && plan.items && plan.items.length > 0) {
            sidebarProvider?.setPlan(plan);
            const provenCount = plan.items.filter((i) => i.proven).length;
            const resp = await vscode.window.showInformationMessage(`AutoResolve completed: ${provenCount} proven fix(es) generated.`, "View Diff");
            if (resp === "View Diff") {
                await diffViewer?.showFixDiff(plan.items[0], 0);
            }
        }
        else {
            vscode.window.showInformationMessage("AutoResolve completed: No fixes proven.");
        }
    }
    catch (err) {
        statusBarItem.text = "$(shield) AutoResolve";
        vscode.window.showErrorMessage(`AutoResolve execution error: ${err.message}`);
    }
}
async function runScan() {
    if (!mcpClient) {
        return;
    }
    vscode.window.withProgress({
        location: vscode.ProgressLocation.Notification,
        title: "Running AutoResolve AST Scan...",
        cancellable: false,
    }, async () => {
        try {
            const findings = await mcpClient.scan(".");
            sidebarProvider?.setFindings(findings);
            if (findings.length === 0) {
                vscode.window.showInformationMessage("AutoResolve AST scan passed: 0 anti-patterns found.");
            }
            else {
                vscode.window.showWarningMessage(`AutoResolve AST scan found ${findings.length} anti-pattern(s).`);
            }
        }
        catch (e) {
            vscode.window.showErrorMessage(`Scan failed: ${e.message}`);
        }
    });
}
function deactivate() {
    mcpClient?.stop();
    pipelineRunner?.cancel();
}
//# sourceMappingURL=extension.js.map