import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { DiffViewer, AutoResolveContentProvider } from "./diffViewer";
import { McpClient } from "./mcpClient";
import { PipelineRunner } from "./pipelineRunner";
import { SidebarProvider } from "./sidebarProvider";
import { Plan, PlanItem } from "./types";

let mcpClient: McpClient | null = null;
let pipelineRunner: PipelineRunner | null = null;
let diffViewer: DiffViewer | null = null;
let sidebarProvider: SidebarProvider | null = null;
let statusBarItem: vscode.StatusBarItem;

function resolveBinaryPath(workspaceRoot: string): string {
  const config = vscode.workspace.getConfiguration("autoresolve");
  const configured = config.get<string>("cliPath");
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

export function activate(context: vscode.ExtensionContext) {
  const workspaceFolders = vscode.workspace.workspaceFolders;
  const workspaceRoot = workspaceFolders && workspaceFolders.length > 0
    ? workspaceFolders[0].uri.fsPath
    : process.cwd();

  const binaryPath = resolveBinaryPath(workspaceRoot);

  // 1. Content provider for diff previews
  const contentProvider = new AutoResolveContentProvider();
  context.subscriptions.push(
    vscode.workspace.registerTextDocumentContentProvider(
      AutoResolveContentProvider.scheme,
      contentProvider
    )
  );

  diffViewer = new DiffViewer(workspaceRoot, contentProvider);

  // 2. MCP client
  mcpClient = new McpClient(binaryPath, workspaceRoot);

  // 3. Pipeline runner
  pipelineRunner = new PipelineRunner(binaryPath, workspaceRoot);

  // 4. Sidebar Webview Provider
  sidebarProvider = new SidebarProvider(
    context.extensionUri,
    async (file: string) => {
      await runFixOnFile(file);
    },
    async () => {
      await runScan();
    },
    async (item: PlanItem, idx: number) => {
      await diffViewer?.showFixDiff(item, idx);
    }
  );

  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      "autoresolve.sidebar",
      sidebarProvider
    )
  );

  // 5. Status bar item
  statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  statusBarItem.command = "autoresolve.runFix";
  statusBarItem.text = "$(shield) AutoResolve";
  statusBarItem.tooltip = "Run AutoResolve on Active Python File";
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  // 6. Commands
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
          await diffViewer?.showFixDiff(plan.items[0], 0);
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

  statusBarItem.text = "$(sync~spin) AutoResolve Running...";

  try {
    const plan = await pipelineRunner.runFix(
      relPath,
      (progress) => {
        sidebarProvider?.updateProgress(progress);
      },
      (ev) => {
        sidebarProvider?.addEvent(ev);
      }
    );

    statusBarItem.text = "$(shield) AutoResolve";

    if (plan && plan.items && plan.items.length > 0) {
      sidebarProvider?.setPlan(plan);
      const provenCount = plan.items.filter((i) => i.proven).length;
      const resp = await vscode.window.showInformationMessage(
        `AutoResolve completed: ${provenCount} proven fix(es) generated.`,
        "View Diff"
      );
      if (resp === "View Diff") {
        await diffViewer?.showFixDiff(plan.items[0], 0);
      }
    } else {
      vscode.window.showInformationMessage("AutoResolve completed: No fixes proven.");
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
