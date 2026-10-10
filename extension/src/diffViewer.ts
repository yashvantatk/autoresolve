import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { Edit, Plan, PlanItem } from "./types";

export class AutoResolveContentProvider implements vscode.TextDocumentContentProvider {
  public static scheme = "autoresolve-preview";
  private onDidChangeEvent = new vscode.EventEmitter<vscode.Uri>();
  public onDidChange = this.onDidChangeEvent.event;

  private previewContents = new Map<string, string>();

  public setPreviewContent(uri: vscode.Uri, content: string): void {
    this.previewContents.set(uri.toString(), content);
    this.onDidChangeEvent.fire(uri);
  }

  public provideTextDocumentContent(uri: vscode.Uri): string {
    return this.previewContents.get(uri.toString()) || "";
  }
}

export class DiffViewer {
  constructor(
    private readonly workspaceRoot: string,
    private readonly provider: AutoResolveContentProvider
  ) {}

  public async showFixDiff(item: PlanItem, index: number): Promise<void> {
    if (!item.edits || item.edits.length === 0) {
      vscode.window.showInformationMessage(`Plan item "${item.title}" contains no edits.`);
      return;
    }

    const firstEdit = item.edits[0];
    const targetFilePath = path.isAbsolute(firstEdit.file)
      ? firstEdit.file
      : path.join(this.workspaceRoot, firstEdit.file);

    if (!fs.existsSync(targetFilePath)) {
      vscode.window.showErrorMessage(`Target file not found: ${targetFilePath}`);
      return;
    }

    const originalContent = fs.readFileSync(targetFilePath, "utf8");
    let patchedContent = originalContent;

    for (const edit of item.edits) {
      if (edit.file === firstEdit.file) {
        if (!patchedContent.includes(edit.search)) {
          vscode.window.showWarningMessage(
            `Edit search target could not be matched in ${edit.file}`
          );
          continue;
        }
        patchedContent = patchedContent.replace(edit.search, edit.replace);
      }
    }

    const originalUri = vscode.Uri.file(targetFilePath);
    const previewUri = vscode.Uri.parse(
      `${AutoResolveContentProvider.scheme}://${targetFilePath}?item=${index}&t=${Date.now()}`
    );

    this.provider.setPreviewContent(previewUri, patchedContent);

    const title = `${path.basename(targetFilePath)}: [${item.proven ? "PROVEN" : "UNPROVEN"}] ${item.title}`;

    await vscode.commands.executeCommand("vscode.diff", originalUri, previewUri, title);

    const action = await vscode.window.showInformationMessage(
      `AutoResolve Fix: "${item.title}" (${item.proven ? "PROVEN" : "UNPROVEN"})`,
      "Accept Fix",
      "Reject Fix"
    );

    if (action === "Accept Fix") {
      await this.applyItemEdits(item);
    } else if (action === "Reject Fix") {
      vscode.window.showInformationMessage(`Fix rejected: "${item.title}"`);
    }
  }

  public async applyItemEdits(item: PlanItem): Promise<boolean> {
    const wsEdit = new vscode.WorkspaceEdit();

    for (const edit of item.edits) {
      const targetFilePath = path.isAbsolute(edit.file)
        ? edit.file
        : path.join(this.workspaceRoot, edit.file);

      const targetUri = vscode.Uri.file(targetFilePath);
      const doc = await vscode.workspace.openTextDocument(targetUri);
      const text = doc.getText();

      const offset = text.indexOf(edit.search);
      if (offset === -1) {
        vscode.window.showErrorMessage(`Could not find search block in ${edit.file}`);
        return false;
      }

      const startPos = doc.positionAt(offset);
      const endPos = doc.positionAt(offset + edit.search.length);
      const range = new vscode.Range(startPos, endPos);

      wsEdit.replace(targetUri, range, edit.replace);
    }

    const applied = await vscode.workspace.applyEdit(wsEdit);
    if (applied) {
      vscode.window.showInformationMessage(`Applied fix: "${item.title}"`);
      await vscode.workspace.saveAll();
    } else {
      vscode.window.showErrorMessage(`Failed to apply edits for: "${item.title}"`);
    }
    return applied;
  }
}
