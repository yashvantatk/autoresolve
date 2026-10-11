import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { DiffViewer, AutoResolveContentProvider } from "./diffViewer";
import { McpClient } from "./mcpClient";
import { PipelineRunner } from "./pipelineRunner";
import { SidebarProvider } from "./sidebarProvider";
import { Plan, PlanItem } from "./types";
import { ensureBinary, verifyDocker } from "./binaryManager";

let extensionContext: vscode.ExtensionContext | null = null;
let mcpClient: McpClient | null = null;
let pipelineRunner: PipelineRunner | null = null;
let diffViewer: DiffViewer | null = null;
let sidebarProvider: SidebarProvider | null = null;
let statusBarItem: vscode.StatusBarItem;
let workspaceRoot: string = process.cwd();
let currentRunTargetFile: string | null = null;

export async function getExecutionEnv(context: vscode.ExtensionContext): Promise<NodeJS.ProcessEnv> {
  const config = vscode.workspace.getConfiguration("autoresolve");
  const provider = config.get<string>("provider", "gemini");
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    AUTORESOLVE_PROVIDER: provider,
  };

  if (provider === "gemini") {
    const secretKey = await context.secrets.get("autoresolve.geminiApiKey");
    if (secretKey && secretKey.trim().length > 0) {
      env.GEMINI_API_KEY = secretKey.trim();
    } else {
      delete env.GEMINI_API_KEY;
    }
  }

  return env;
}

export async function ensureApiKey(context: vscode.ExtensionContext): Promise<boolean> {
  const config = vscode.workspace.getConfiguration("autoresolve");
  const provider = config.get<string>("provider", "gemini");
  if (provider !== "gemini") {
    return true;
  }

  const secretKey = await context.secrets.get("autoresolve.geminiApiKey");
  const hasValidKey = Boolean(secretKey && secretKey.trim().length > 0);
  if (hasValidKey) {
    return true;
  }

  const choice = await vscode.window.showWarningMessage(
    "AutoResolve: Gemini API key is required to run AI review and fixes.",
    "Enter API Key",
    "Get API Key"
  );

  if (choice === "Get API Key") {
    vscode.env.openExternal(vscode.Uri.parse("https://aistudio.google.com/app/apikey"));
    return false;
  } else if (choice === "Enter API Key") {
    await vscode.commands.executeCommand("autoresolve.setApiKey");
    const updated = await context.secrets.get("autoresolve.geminiApiKey");
    return Boolean(updated && updated.trim().length > 0);
  }

  return false;
}

export async function activate(context: vscode.ExtensionContext) {
  extensionContext = context;

  const workspaceFolders = vscode.workspace.workspaceFolders;
  workspaceRoot = workspaceFolders && workspaceFolders.length > 0
    ? workspaceFolders[0].uri.fsPath
    : process.cwd();

  // 1. Verify Docker in background (only warns if configured for Cloud AI or Local AI)
  verifyDocker(context);

  // 2. Automated Binary Management
  const initialBinary = await ensureBinary(context, workspaceRoot);

  // 3. Content provider for diff previews
  const contentProvider = new AutoResolveContentProvider();
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(
      AutoResolveContentProvider.scheme,
      contentProvider
    )
  );

  diffViewer = new DiffViewer(workspaceRoot, contentProvider);

  // 4. MCP client
  mcpClient = new McpClient(initialBinary, workspaceRoot, context);

  // 5. Pipeline runner
  pipelineRunner = new PipelineRunner(initialBinary, workspaceRoot, context);

  // Watch for configuration changes
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration(async (e) => {
      if (e.affectsConfiguration("autoresolve.cliPath")) {
        const updatedBin = await ensureBinary(context, workspaceRoot);
        mcpClient?.setBinaryPath(updatedBin);
        pipelineRunner?.setBinaryPath(updatedBin);
      }
      if (e.affectsConfiguration("autoresolve.provider")) {
        verifyDocker(context);
      }
    })
  );

  // 6. Sidebar Webview Provider
  sidebarProvider = new SidebarProvider(
    context.extensionUri,
    async (file: string) => {
      await runFixOnFile(file);
    },
    async () => {
      await runScan();
    },
    async (item: PlanItem, idx: number, targetFile?: string) => {
      const activeDoc = vscode.window.activeTextEditor?.document;
      const activeFsPath = activeDoc && (activeDoc.languageId === "python" || activeDoc.fileName.endsWith(".py"))
        ? activeDoc.uri.fsPath
        : undefined;
      const resolvedTarget = targetFile || activeFsPath || currentRunTargetFile || undefined;
      await diffViewer?.showFixDiff(item, idx, resolvedTarget);
    }
  );

  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      "autoresolve.sidebar",
      sidebarProvider
    )
  );

  // 7. Status bar item
  statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  statusBarItem.command = "autoresolve.runFix";
  statusBarItem.text = "$(shield) AutoResolve";
  statusBarItem.tooltip = "Run AutoResolve on Active Python File";
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  // 8. Register Secret Storage Command: autoresolve.setApiKey
  context.subscriptions.push(
    vscode.commands.registerCommand("autoresolve.setApiKey", async () => {
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
        verifyDocker(context);
        return;
      }

      await context.secrets.store("autoresolve.geminiApiKey", key.trim());
      vscode.window.showInformationMessage("AutoResolve: Gemini API key securely saved in SecretStorage.");
      verifyDocker(context);
    })
  );

  // 9. Commands
  context.subscriptions.push(
    vscode.commands.registerCommand("autoresolve.runFix", async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "python") {
        vscode.window.showWarningMessage("Please open a Python file to run AutoResolve.");
        return;
      }
      const relPath = vscode.workspace.asRelativePath(editor.document.uri);
      await runFixOnFile(relPath);
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("autoresolve.runScan", async () => {
      await runScan();
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("autoresolve.reviewActiveFile", async () => {
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
      vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: `AutoResolve: Reviewing ${relPath}...`,
          cancellable: false,
        },
        async () => {
          try {
            const report = await mcpClient?.review(relPath);
            const doc = await vscode.workspace.openTextDocument({
              content: report || "No review findings.",
              language: "markdown",
            });
            await vscode.window.showTextDocument(doc);
          } catch (e: any) {
            vscode.window.showErrorMessage(`AutoResolve review failed: ${e.message}`);
          }
        }
      );
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("autoresolve.viewPlanDiffs", async () => {
      const planPath = path.join(workspaceRoot, ".autoresolve", "plan.json");
      if (!fs.existsSync(planPath)) {
        vscode.window.showInformationMessage("No plan.json found. Run AutoResolve first.");
        return;
      }
      try {
        const plan: Plan = JSON.parse(fs.readFileSync(planPath, "utf8"));
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
        } else {
          vscode.window.showInformationMessage("Plan contains 0 items.");
        }
      } catch (e: any) {
        vscode.window.showErrorMessage(`Failed to read plan: ${e.message}`);
      }
    })
  );

  // Listen to active editor switches
  context.subscriptions.push(
    vscode.window.onDidChangeActiveTextEditor(() => {
      sidebarProvider?.updateActiveFile();
    })
  );
}

async function runFixOnFile(relPath: string) {
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
    const plan = await pipelineRunner.runFix(
      relPath,
      (progress) => {
        sidebarProvider?.updateProgress(progress);
      },
      (ev) => {
        sidebarProvider?.addEvent(ev);
      },
      customEnv
    );

    statusBarItem.text = "$(shield) AutoResolve";

    if (plan) {
      // Ensure target_file is strictly bound to the analyzed file
      if (plan.target_file) {
        plan.target_file = path.isAbsolute(plan.target_file)
          ? plan.target_file
          : path.join(workspaceRoot, plan.target_file);
      } else {
        plan.target_file = targetFsPath;
      }
    }

    if (plan && plan.items && plan.items.length > 0) {
      sidebarProvider?.setPlan(plan);
      const provenCount = plan.items.filter((i) => i.proven).length;
      const targetDisplay = path.basename(plan.target_file || targetFsPath);
      const resp = await vscode.window.showInformationMessage(
        `AutoResolve completed: ${provenCount} proven fix(es) generated for ${targetDisplay}.`,
        "View Diff"
      );
      if (resp === "View Diff") {
        await diffViewer?.showFixDiff(plan.items[0], 0, plan.target_file || targetFsPath);
      }
    } else {
      sidebarProvider?.setPlan({ target_file: targetFsPath, items: [] });
      vscode.window.showInformationMessage(`AutoResolve completed for ${path.basename(targetFsPath)}: No fixes proven.`);
    }
  } catch (err: any) {
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
  vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: "Running AutoResolve AST Scan...",
      cancellable: false,
    },
    async () => {
      try {
        const findings = await mcpClient!.scan(".");
        sidebarProvider?.setFindings(findings);
        if (findings.length === 0) {
          vscode.window.showInformationMessage("AutoResolve AST scan passed: 0 anti-patterns found.");
        } else {
          vscode.window.showWarningMessage(`AutoResolve AST scan found ${findings.length} anti-pattern(s).`);
        }
      } catch (e: any) {
        vscode.window.showErrorMessage(`Scan failed: ${e.message}`);
      }
    }
  );
}

export function deactivate() {
  mcpClient?.stop();
  pipelineRunner?.cancel();
}
