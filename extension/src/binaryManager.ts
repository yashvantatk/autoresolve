import * as cp from "child_process";
import * as fs from "fs";
import * as http from "http";
import * as https from "https";
import * as path from "path";
import * as vscode from "vscode";

const RELEASE_BASE_URL = "https://github.com/yashvantatk/autoresolve/releases/latest/download/";

export function getBinaryAssetName(): string {
  const platform = process.platform === "win32" ? "windows" : process.platform;
  const arch = process.arch === "x64" ? "x86_64" : process.arch === "arm64" ? "aarch64" : process.arch;
  const ext = process.platform === "win32" ? ".exe" : "";
  return `autoresolve-cli-${platform}-${arch}${ext}`;
}

export function getManagedBinaryPath(globalStoragePath: string): string {
  const ext = process.platform === "win32" ? ".exe" : "";
  return path.join(globalStoragePath, `autoresolve-cli${ext}`);
}

function downloadStream(url: string, destPath: string, redirectCount = 0): Promise<void> {
  if (redirectCount > 5) {
    return Promise.reject(new Error("Too many redirects while downloading binary"));
  }

  return new Promise((resolve, reject) => {
    let parsed: URL;
    try {
      parsed = new URL(url);
    } catch (e) {
      return reject(new Error(`Invalid download URL: ${url}`));
    }

    const client = parsed.protocol === "http:" ? http : https;

    const req = client.get(url, { headers: { "User-Agent": "autoresolve-vscode" } }, (res) => {
      // Follow HTTP 301, 302, 307, 308 redirects (GitHub Releases redirect to AWS S3/CDN)
      if (res.statusCode && res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        let redirectUrl = res.headers.location;
        if (!redirectUrl.startsWith("http")) {
          redirectUrl = new URL(redirectUrl, url).toString();
        }
        res.resume();
        downloadStream(redirectUrl, destPath, redirectCount + 1)
          .then(resolve)
          .catch(reject);
        return;
      }

      if (res.statusCode !== 200) {
        res.resume();
        reject(new Error(`Download failed with HTTP ${res.statusCode}: ${res.statusMessage}`));
        return;
      }

      const fileStream = fs.createWriteStream(destPath);
      res.pipe(fileStream);

      fileStream.on("finish", () => {
        fileStream.close((err) => {
          if (err) {
            reject(err);
          } else {
            resolve();
          }
        });
      });

      fileStream.on("error", (err) => {
        fs.unlink(destPath, () => {});
        reject(err);
      });
    });

    req.on("error", (err) => {
      fs.unlink(destPath, () => {});
      reject(err);
    });

    req.setTimeout(30000, () => {
      req.destroy();
      fs.unlink(destPath, () => {});
      reject(new Error("Binary download timed out after 30 seconds"));
    });
  });
}

function findLocalFallbackBinary(workspaceRoot?: string): string | null {
  const config = vscode.workspace.getConfiguration("autoresolve");
  const configured = config.get<string>("cliPath");
  if (configured && fs.existsSync(configured)) {
    return configured;
  }

  if (workspaceRoot) {
    const candidates = [
      path.join(workspaceRoot, "target", "release", "autoresolve-cli"),
      path.join(workspaceRoot, "target", "debug", "autoresolve-cli"),
      path.join(workspaceRoot, "..", "target", "release", "autoresolve-cli"),
      path.join(workspaceRoot, "..", "target", "debug", "autoresolve-cli"),
      path.join(workspaceRoot, "target", "release", "autoresolve-cli.exe"),
      path.join(workspaceRoot, "target", "debug", "autoresolve-cli.exe"),
    ];
    for (const c of candidates) {
      if (fs.existsSync(c)) {
        return path.resolve(c);
      }
    }
  }

  return null;
}

export async function ensureBinary(
  context: vscode.ExtensionContext,
  workspaceRoot?: string
): Promise<string> {
  // 1. Explicit user configuration takes precedence if specified
  const config = vscode.workspace.getConfiguration("autoresolve");
  const configuredPath = config.get<string>("cliPath");
  if (configuredPath && fs.existsSync(configuredPath)) {
    return configuredPath;
  }

  // 2. Check if managed binary already exists in globalStorageUri
  const globalStoragePath = context.globalStorageUri.fsPath;
  const managedBinaryPath = getManagedBinaryPath(globalStoragePath);

  if (fs.existsSync(managedBinaryPath)) {
    // Ensure executable permissions
    if (process.platform !== "win32") {
      try {
        fs.chmodSync(managedBinaryPath, 0o755);
      } catch {
        // Ignore chmod errors if permissions already sufficient
      }
    }
    return managedBinaryPath;
  }

  // 3. Binary not found; prepare storage directory and attempt download
  if (!fs.existsSync(globalStoragePath)) {
    fs.mkdirSync(globalStoragePath, { recursive: true });
  }

  const assetName = getBinaryAssetName();
  const downloadUrl = `${RELEASE_BASE_URL}${assetName}`;
  const tmpDownloadPath = path.join(globalStoragePath, `autoresolve-cli-${Date.now()}.tmp`);

  let downloadSuccess = false;
  let downloadError: any = null;

  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: "AutoResolve: Downloading CLI binary...",
      cancellable: false,
    },
    async (progress) => {
      progress.report({ message: `Fetching ${assetName}...` });
      try {
        await downloadStream(downloadUrl, tmpDownloadPath);
        // Rename atomically
        fs.renameSync(tmpDownloadPath, managedBinaryPath);
        if (process.platform !== "win32") {
          fs.chmodSync(managedBinaryPath, 0o755);
        }
        downloadSuccess = true;
      } catch (err: any) {
        downloadError = err;
        try {
          if (fs.existsSync(tmpDownloadPath)) {
            fs.unlinkSync(tmpDownloadPath);
          }
        } catch {}
      }
    }
  );

  if (downloadSuccess && fs.existsSync(managedBinaryPath)) {
    vscode.window.showInformationMessage("AutoResolve: CLI binary downloaded and verified successfully.");
    return managedBinaryPath;
  }

  // 4. Download failed: check for local workspace or debug build as fallback
  const localFallback = findLocalFallbackBinary(workspaceRoot);
  if (localFallback) {
    try {
      // Copy local fallback into managed path so global storage is primed
      fs.copyFileSync(localFallback, managedBinaryPath);
      if (process.platform !== "win32") {
        fs.chmodSync(managedBinaryPath, 0o755);
      }
      vscode.window.showInformationMessage(`AutoResolve: Using local binary fallback (${path.basename(localFallback)}).`);
      return managedBinaryPath;
    } catch {
      return localFallback;
    }
  }

  // 5. Neither download nor local fallback available: alert user
  const errMsg = downloadError ? downloadError.message : "Download failed";
  vscode.window.showErrorMessage(
    `AutoResolve: Could not download pre-compiled binary (${errMsg}) and no local binary was found. You can build it with 'cargo build' or set 'autoresolve.cliPath'.`
  );

  return managedBinaryPath;
}

export async function verifyDocker(context?: vscode.ExtensionContext): Promise<void> {
  const config = vscode.workspace.getConfiguration("autoresolve");
  const provider = config.get<string>("provider", "gemini");

  let apiKey: string | undefined = undefined;
  if (context && provider === "gemini") {
    try {
      const storedKey = await context.secrets.get("autoresolve.geminiApiKey");
      if (storedKey) {
        apiKey = storedKey;
      }
    } catch {
      // Ignore secret storage access errors
    }
  }

  const hasValidKey = Boolean(apiKey && apiKey.trim().length > 0);
  const needsDocker = provider === "ollama" || (provider === "gemini" && hasValidKey);
  if (!needsDocker) {
    return;
  }

  cp.exec("docker info", (error) => {
    if (error) {
      vscode.window.showWarningMessage(
        "AutoResolve requires Docker to run its secure verification sandbox.",
        "Get Docker"
      ).then((selection) => {
        if (selection === "Get Docker") {
          vscode.env.openExternal(vscode.Uri.parse("https://docs.docker.com/get-docker/"));
        }
      });
    }
  });
}
