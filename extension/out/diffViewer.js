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
exports.DiffViewer = exports.AutoResolveContentProvider = void 0;
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
const vscode = __importStar(require("vscode"));
class AutoResolveContentProvider {
    static scheme = "autoresolve-preview";
    onDidChangeEvent = new vscode.EventEmitter();
    onDidChange = this.onDidChangeEvent.event;
    previewContents = new Map();
    setPreviewContent(uri, content) {
        this.previewContents.set(uri.toString(), content);
        this.onDidChangeEvent.fire(uri);
    }
    provideTextDocumentContent(uri) {
        return this.previewContents.get(uri.toString()) || "";
    }
}
exports.AutoResolveContentProvider = AutoResolveContentProvider;
class DiffViewer {
    workspaceRoot;
    provider;
    constructor(workspaceRoot, provider) {
        this.workspaceRoot = workspaceRoot;
        this.provider = provider;
    }
    async showFixDiff(item, index) {
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
                    vscode.window.showWarningMessage(`Edit search target could not be matched in ${edit.file}`);
                    continue;
                }
                patchedContent = patchedContent.replace(edit.search, edit.replace);
            }
        }
        const originalUri = vscode.Uri.file(targetFilePath);
        const previewUri = vscode.Uri.parse(`${AutoResolveContentProvider.scheme}://${targetFilePath}?item=${index}&t=${Date.now()}`);
        this.provider.setPreviewContent(previewUri, patchedContent);
        const title = `${path.basename(targetFilePath)}: [${item.proven ? "PROVEN" : "UNPROVEN"}] ${item.title}`;
        await vscode.commands.executeCommand("vscode.diff", originalUri, previewUri, title);
        const action = await vscode.window.showInformationMessage(`AutoResolve Fix: "${item.title}" (${item.proven ? "PROVEN" : "UNPROVEN"})`, "Accept Fix", "Reject Fix");
        if (action === "Accept Fix") {
            await this.applyItemEdits(item);
        }
        else if (action === "Reject Fix") {
            vscode.window.showInformationMessage(`Fix rejected: "${item.title}"`);
        }
    }
    async applyItemEdits(item) {
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
        }
        else {
            vscode.window.showErrorMessage(`Failed to apply edits for: "${item.title}"`);
        }
        return applied;
    }
}
exports.DiffViewer = DiffViewer;
//# sourceMappingURL=diffViewer.js.map