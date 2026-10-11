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
exports.getExecutionEnv = getExecutionEnv;
exports.ensureApiKey = ensureApiKey;
exports.activate = activate;
exports.deactivate = deactivate;
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
const vscode = __importStar(require("vscode"));
const diffViewer_1 = require("./diffViewer");
const mcpClient_1 = require("./mcpClient");
const pipelineRunner_1 = require("./pipelineRunner");
const sidebarProvider_1 = require("./sidebarProvider");
const binaryManager_1 = require("./binaryManager");
let extensionContext = null;
let mcpClient = null;
let pipelineRunner = null;
let diffViewer = null;
let sidebarProvider = null;
let statusBarItem;
let workspaceRoot = process.cwd();
let currentRunTargetFile = null;
async function getExecutionEnv(context) {
    const config = vscode.workspace.getConfiguration("autoresolve");
    const provider = config.get("provider", "gemini");
    const env = {
        ...process.env,
        AUTORESOLVE_PROVIDER: provider,
    };
    if (provider === "gemini") {
        const secretKey = await context.secrets.get("autoresolve.geminiApiKey");
        if (secretKey && secretKey.trim().length > 0) {
            env.GEMINI_API_KEY = secretKey.trim();
        }
        else {
            delete env.GEMINI_API_KEY;
        }
    }
    return env;
}
async function ensureApiKey(context) {
    const config = vscode.workspace.getConfiguration("autoresolve");
    const provider = config.get("provider", "gemini");
    if (provider !== "gemini") {
        return true;
    }
    const secretKey = await context.secrets.get("autoresolve.geminiApiKey");
    const hasValidKey = Boolean(secretKey && secretKey.trim().length > 0);
    if (hasValidKey) {
        return true;
    }
    const choice = await vscode.window.showWarningMessage("AutoResolve: Gemini API key is required to run AI review and fixes.", "Enter API Key", "Get API Key");
    if (choice === "Get API Key") {
        vscode.env.openExternal(vscode.Uri.parse("https://aistudio.google.com/app/apikey"));
        return false;
    }
    else if (choice === "Enter API Key") {
        await vscode.commands.executeCommand("autoresolve.setApiKey");
        const updated = await context.secrets.get("autoresolve.geminiApiKey");
        return Boolean(updated && updated.trim().length > 0);
    }
    return false;
}
async function activate(context) {
    extensionContext = context;
    const workspaceFolders = vscode.workspace.workspaceFolders;
    workspaceRoot = workspaceFolders && workspaceFolders.length > 0
        ? workspaceFolders[0].uri.fsPath
        : process.cwd();
    // 1. Verify Docker in background (only warns if configured for Cloud AI or Local AI)
    (0, binaryManager_1.verifyDocker)(context);
    // 2. Automated Binary Management
    const initialBinary = await (0, binaryManager_1.ensureBinary)(context, workspaceRoot);
    // 3. Content provider for diff previews
    const contentProvider = new diffViewer_1.AutoResolveContentProvider();
    context.subscriptions.push(vscode.workspace.registerTextDocumentContentProvider(diffViewer_1.AutoResolveContentProvider.scheme, contentProvider));
    diffViewer = new diffViewer_1.DiffViewer(workspaceRoot, contentProvider);
    // 4. MCP client
    mcpClient = new mcpClient_1.McpClient(initialBinary, workspaceRoot, context);
    // 5. Pipeline runner
    pipelineRunner = new pipelineRunner_1.PipelineRunner(initialBinary, workspaceRoot, context);
    // Watch for configuration changes
    context.subscriptions.push(vscode.workspace.onDidChangeConfiguration(async (e) => {
        if (e.affectsConfiguration("autoresolve.cliPath")) {
            const updatedBin = await (0, binaryManager_1.ensureBinary)(context, workspaceRoot);
            mcpClient?.setBinaryPath(updatedBin);
            pipelineRunner?.setBinaryPath(updatedBin);
        }
        if (e.affectsConfiguration("autoresolve.provider")) {
            (0, binaryManager_1.verifyDocker)(context);
        }
    }));
    // 6. Sidebar Webview Provider
    sidebarProvider = new sidebarProvider_1.SidebarProvider(context.extensionUri, async (file) => {
        await runFixOnFile(file);
    }, async () => {
        await runScan();
    }, async (item, idx, targetFile) => {
        const activeDoc = vscode.window.activeTextEditor?.document;
        const activeFsPath = activeDoc && (activeDoc.languageId === "python" || activeDoc.fileName.endsWith(".py"))
            ? activeDoc.uri.fsPath
            : undefined;
        const resolvedTarget = targetFile || activeFsPath || currentRunTargetFile || undefined;
        await diffViewer?.showFixDiff(item, idx, resolvedTarget);
    });
    context.subscriptions.push(vscode.window.registerWebviewViewProvider("autoresolve.sidebar", sidebarProvider));
    // 7. Status bar item
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    statusBarItem.command = "autoresolve.runFix";
    statusBarItem.text = "$(shield) AutoResolve";
    statusBarItem.tooltip = "Run AutoResolve on Active Python File";
    statusBarItem.show();
    context.subscriptions.push(statusBarItem);
    // 8. Register Secret Storage Command: autoresolve.setApiKey
    context.subscriptions.push(vscode.commands.registerCommand("autoresolve.setApiKey", async () => {
        const existingKey = await context.secrets.get("autoresolve.geminiApiKey");
        const key = await vscode.window.showInputBox({
            title: "AutoResolve: Set Gemini API Key",
            prompt: "Enter your Gemini API key to securely save it in VS Code SecretStorage",
            placeHolder: existingKey ? "•••••••••••••••• (Leave blank to keep existing)" : "AIzaSy...",
            password: true,
            ignoreFocusOut: true,
        });
        if (key === undefined) {
            return;
        }
        if (key.trim() === "") {
            await context.secrets.delete("autoresolve.geminiApiKey");
            vscode.window.showInformationMessage("AutoResolve: Gemini API key cleared. Deterministic Mode will be used.");
            (0, binaryManager_1.verifyDocker)(context);
            return;
        }
        await context.secrets.store("autoresolve.geminiApiKey", key.trim());
        vscode.window.showInformationMessage("AutoResolve: Gemini API key securely saved in SecretStorage.");
        (0, binaryManager_1.verifyDocker)(context);
    }));
    // 9. Commands
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
        if (extensionContext) {
            const hasKey = await ensureApiKey(extensionContext);
            if (!hasKey) {
                return;
            }
            const env = await getExecutionEnv(extensionContext);
            mcpClient?.setEnv(env);
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
                const activeDoc = vscode.window.activeTextEditor?.document;
                const activeFsPath = activeDoc && (activeDoc.languageId === "python" || activeDoc.fileName.endsWith(".py"))
                    ? activeDoc.uri.fsPath
                    : undefined;
                const resolvedTarget = plan.target_file
                    ? (path.isAbsolute(plan.target_file) ? plan.target_file : path.join(workspaceRoot, plan.target_file))
                    : activeFsPath || currentRunTargetFile || undefined;
                await diffViewer?.showFixDiff(plan.items[0], 0, resolvedTarget);
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
    // 1. Explicitly clear previous run state from sidebar and logBox
    sidebarProvider?.clearState();
    // 2. Strict active editor binding: resolve target file strictly for this current run
    const activeEditor = vscode.window.activeTextEditor;
    const activeDocUri = activeEditor?.document.uri;
    const activeFsPath = activeDocUri && (activeEditor.document.languageId === "python" || activeEditor.document.fileName.endsWith(".py"))
        ? activeDocUri.fsPath
        : undefined;
    const targetFsPath = activeFsPath || (path.isAbsolute(relPath) ? relPath : path.join(workspaceRoot, relPath));
    currentRunTargetFile = targetFsPath;
    const customEnv = extensionContext ? await getExecutionEnv(extensionContext) : process.env;
    statusBarItem.text = "$(sync~spin) AutoResolve Running...";
    try {
        const plan = await pipelineRunner.runFix(relPath, (progress) => {
            sidebarProvider?.updateProgress(progress);
        }, (ev) => {
            sidebarProvider?.addEvent(ev);
        }, customEnv);
        statusBarItem.text = "$(shield) AutoResolve";
        if (plan) {
            // Ensure target_file is strictly bound to the analyzed file
            if (plan.target_file) {
                plan.target_file = path.isAbsolute(plan.target_file)
                    ? plan.target_file
                    : path.join(workspaceRoot, plan.target_file);
            }
            else {
                plan.target_file = targetFsPath;
            }
        }
        if (plan && plan.items && plan.items.length > 0) {
            sidebarProvider?.setPlan(plan);
            const provenCount = plan.items.filter((i) => i.proven).length;
            const targetDisplay = path.basename(plan.target_file || targetFsPath);
            const resp = await vscode.window.showInformationMessage(`AutoResolve completed: ${provenCount} proven fix(es) generated for ${targetDisplay}.`, "View Diff");
            if (resp === "View Diff") {
                await diffViewer?.showFixDiff(plan.items[0], 0, plan.target_file || targetFsPath);
            }
        }
        else {
            sidebarProvider?.setPlan({ target_file: targetFsPath, items: [] });
            vscode.window.showInformationMessage(`AutoResolve completed for ${path.basename(targetFsPath)}: No fixes proven.`);
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
    if (extensionContext) {
        const customEnv = await getExecutionEnv(extensionContext);
        mcpClient.setEnv(customEnv);
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