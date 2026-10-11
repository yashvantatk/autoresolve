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

function normalizeLineEndings(str: string): string {
  return str.replace(/\r\n/g, "\n");
}

/**
 * Applies search/replace to a string content with CRLF normalization
 * and fuzzy/trimmed line-by-line fallback.
 */
export function applySearchReplaceToContent(content: string, search: string, replace: string): string | null {
  if (!search) {
    return null;
  }

  // 1. Exact match
  if (content.includes(search)) {
    return content.replace(search, replace);
  }

  // 2. CRLF-normalized exact match
  const normContent = normalizeLineEndings(content);
  const normSearch = normalizeLineEndings(search);
  const normReplace = normalizeLineEndings(replace);

  if (normContent.includes(normSearch)) {
    return normContent.replace(normSearch, normReplace);
  }

  // 3. Fuzzy / Trimmed line matching fallback (trimEnd on each line)
  const contentLines = normContent.split("\n");
  const searchLines = normSearch.split("\n");

  if (searchLines.length > 0 && searchLines.length <= contentLines.length) {
    const trimmedSearch = searchLines.map(l => l.trimEnd());
    const matches: number[] = [];

    for (let i = 0; i <= contentLines.length - searchLines.length; i++) {
      let matchesAll = true;
      for (let j = 0; j < searchLines.length; j++) {
        if (contentLines[i + j].trimEnd() !== trimmedSearch[j]) {
          matchesAll = false;
          break;
        }
      }
      if (matchesAll) {
        matches.push(i);
      }
    }

    if (matches.length === 1) {
      const startLine = matches[0];
      const endLine = startLine + searchLines.length;

      const newLines: string[] = [];
      for (let i = 0; i < contentLines.length; i++) {
        if (i === startLine) {
          newLines.push(normReplace);
        } else if (i > startLine && i < endLine) {
          // skip replaced lines
        } else {
          newLines.push(contentLines[i]);
        }
      }
      let result = newLines.join("\n");
      if (normContent.endsWith("\n") && !result.endsWith("\n")) {
        result += "\n";
      }
      return result;
    }
  }

  return null;
}

/**
 * Finds the vscode.Range in a document for the given search block.
 * 1. Checks exact match.
 * 2. Checks line-ending normalized match.
 * 3. Falls back to line-by-line comparison with trailing whitespace trimmed.
 */
export function findSearchRangeInDocument(doc: vscode.TextDocument, search: string): vscode.Range | null {
  if (!search) {
    return null;
  }

  const text = doc.getText();

  // 1. Direct match in raw text
  const directIdx = text.indexOf(search);
  if (directIdx !== -1) {
    return new vscode.Range(doc.positionAt(directIdx), doc.positionAt(directIdx + search.length));
  }

  // 2. Line-by-line comparison with CRLF normalization & trailing whitespace trimming
  const searchLines = normalizeLineEndings(search).split("\n");
  const trimmedSearchLines = searchLines.map(l => l.trimEnd());
  const docLineCount = doc.lineCount;

  if (searchLines.length === 0 || searchLines.length > docLineCount) {
    return null;
  }

  // Try matching lines without trimming first
  for (let i = 0; i <= docLineCount - searchLines.length; i++) {
    let exactLineMatch = true;
    for (let j = 0; j < searchLines.length; j++) {
      const docLineText = doc.lineAt(i + j).text.replace(/\r/g, "");
      if (docLineText !== searchLines[j]) {
        exactLineMatch = false;
        break;
      }
    }
    if (exactLineMatch) {
      const startPos = new vscode.Position(i, 0);
      const lastLine = i + searchLines.length - 1;
      const endPos = new vscode.Position(lastLine, doc.lineAt(lastLine).text.length);
      return new vscode.Range(startPos, endPos);
    }
  }

  // 3. Fuzzy trimmed match fallback (trimEnd on each line)
  const matches: number[] = [];
  for (let i = 0; i <= docLineCount - searchLines.length; i++) {
    let trimmedMatch = true;
    for (let j = 0; j < searchLines.length; j++) {
      const docLineTrimmed = doc.lineAt(i + j).text.replace(/\r/g, "").trimEnd();
      if (docLineTrimmed !== trimmedSearchLines[j]) {
        trimmedMatch = false;
        break;
      }
    }
    if (trimmedMatch) {
      matches.push(i);
    }
  }

  if (matches.length === 1) {
    const startLine = matches[0];
    const endLine = startLine + searchLines.length - 1;
    const startPos = new vscode.Position(startLine, 0);
    const endPos = new vscode.Position(endLine, doc.lineAt(endLine).text.length);
    return new vscode.Range(startPos, endPos);
  }

  return null;
}

export class DiffViewer {
  constructor(
    private readonly workspaceRoot: string,
    private readonly provider: AutoResolveContentProvider
  ) {}

  public async showFixDiff(
    item: PlanItem,
    index: number,
    targetFileOverride?: string
  ): Promise<void> {
    if (!item.edits || item.edits.length === 0) {
      vscode.window.showInformationMessage(`Plan item "${item.title}" contains no edits.`);
      return;
    }

    // Determine target file:
    // 1. Explicit targetFileOverride passed from current pipeline run or JSON payload
    // 2. Active text editor if it is a python file
    // 3. File specified in edit.file
    let chosenTarget = targetFileOverride;
    if (!chosenTarget) {
      const activeDoc = vscode.window.activeTextEditor?.document;
      if (activeDoc && (activeDoc.languageId === "python" || activeDoc.fileName.endsWith(".py"))) {
        chosenTarget = activeDoc.uri.fsPath;
      }
    }
    if (!chosenTarget) {
      chosenTarget = item.edits[0].file;
    }

    const targetFilePath = path.isAbsolute(chosenTarget)
      ? chosenTarget
      : path.join(this.workspaceRoot, chosenTarget);

    if (!fs.existsSync(targetFilePath)) {
      vscode.window.showErrorMessage(`Target file not found: ${targetFilePath}`);
      return;
    }

    const originalContent = fs.readFileSync(targetFilePath, "utf8");
    let patchedContent = originalContent;

    for (const edit of item.edits) {
      const updated = applySearchReplaceToContent(patchedContent, edit.search, edit.replace);
      if (!updated) {
        const preview = patchedContent.split("\n").slice(0, 10).join("\n");
        vscode.window.showWarningMessage(
          `Edit search target could not be matched in ${path.basename(targetFilePath)}.\n\nExpected Search Block:\n${edit.search}\n\nFile Preview:\n${preview}`
        );
        continue;
      }
      patchedContent = updated;
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
      await this.applyItemEdits(item, targetFilePath);
    } else if (action === "Reject Fix") {
      vscode.window.showInformationMessage(`Fix rejected: "${item.title}"`);
    }
  }

  public async applyItemEdits(item: PlanItem, targetFileOverride?: string): Promise<boolean> {
    const wsEdit = new vscode.WorkspaceEdit();

    for (const edit of item.edits) {
      let resolvedEditTarget = targetFileOverride;
      if (!resolvedEditTarget) {
        resolvedEditTarget = path.isAbsolute(edit.file)
          ? edit.file
          : path.join(this.workspaceRoot, edit.file);
      } else if (!path.isAbsolute(resolvedEditTarget)) {
        resolvedEditTarget = path.join(this.workspaceRoot, resolvedEditTarget);
      }

      const targetUri = vscode.Uri.file(resolvedEditTarget);
      const doc = await vscode.workspace.openTextDocument(targetUri);

      const range = findSearchRangeInDocument(doc, edit.search);
      if (!range) {
        const previewSlice = doc.getText().split("\n").slice(0, 15).join("\n");
        vscode.window.showErrorMessage(
          `Could not match edit search target in ${path.basename(resolvedEditTarget)}.\n\nExpected Search Block:\n${edit.search}\n\nFile Preview:\n${previewSlice}`
        );
        return false;
      }

      const normReplace = normalizeLineEndings(edit.replace);
      wsEdit.replace(targetUri, range, normReplace);
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
