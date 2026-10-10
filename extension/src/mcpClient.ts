import * as cp from "child_process";
import * as readline from "readline";
import { JsonRpcRequest, JsonRpcResponse, Finding } from "./types";

export class McpClient {
  private process: cp.ChildProcess | null = null;
  private pendingRequests = new Map<number | string, {
    resolve: (val: any) => void;
    reject: (err: Error) => void;
  }>();
  private nextId = 1;
  private initialized = false;

  constructor(
    private readonly binaryPath: string,
    private readonly workspaceRoot: string
  ) {}

  public async start(): Promise<void> {
    if (this.process) {
      return;
    }

    this.process = cp.spawn(this.binaryPath, ["mcp"], {
      cwd: this.workspaceRoot,
      stdio: ["pipe", "pipe", "pipe"],
      env: {
        ...process.env,
      },
    });

    if (!this.process.stdout || !this.process.stdin) {
      throw new Error("Failed to create stdio streams for autoresolve-cli mcp");
    }

    const rl = readline.createInterface({
      input: this.process.stdout,
      terminal: false,
    });

    rl.on("line", (line: string) => {
      const trimmed = line.trim();
      if (!trimmed) {
        return;
      }
      try {
        const res: JsonRpcResponse = JSON.parse(trimmed);
        if (res.id !== undefined && this.pendingRequests.has(res.id)) {
          const { resolve, reject } = this.pendingRequests.get(res.id)!;
          this.pendingRequests.delete(res.id);
          if (res.error) {
            reject(new Error(`MCP error ${res.error.code}: ${res.error.message}`));
          } else {
            resolve(res.result);
          }
        }
      } catch (err) {
        console.error("[MCP] Malformed JSON received from server:", line, err);
      }
    });

    this.process.stderr?.on("data", (data: Buffer) => {
      console.warn(`[MCP Server stderr]: ${data.toString()}`);
    });

    this.process.on("exit", (code, signal) => {
      console.log(`[MCP Server] exited with code ${code}, signal ${signal}`);
      for (const [, p] of this.pendingRequests) {
        p.reject(new Error(`MCP server process terminated (code ${code})`));
      }
      this.pendingRequests.clear();
      this.process = null;
      this.initialized = false;
    });

    await this.initialize();
  }

  private async initialize(): Promise<void> {
    const initRes = await this.sendRequest("initialize", {
      protocolVersion: "2025-06-18",
    });
    this.sendNotification("notifications/initialized");
    this.initialized = true;
  }

  public async callTool(name: string, args: Record<string, any> = {}): Promise<any> {
    if (!this.initialized) {
      await this.start();
    }
    const res = await this.sendRequest("tools/call", {
      name,
      arguments: args,
    });
    const textContent = res?.content?.find((c: any) => c.type === "text")?.text;
    if (res?.isError) {
      throw new Error(textContent || `Tool ${name} failed`);
    }
    return textContent;
  }

  public async scan(path: string = "."): Promise<Finding[]> {
    const raw = await this.callTool("scan", { path });
    try {
      return JSON.parse(raw);
    } catch {
      return [];
    }
  }

  public async symbols(path: string = "."): Promise<any[]> {
    const raw = await this.callTool("symbols", { path });
    try {
      return JSON.parse(raw);
    } catch {
      return [];
    }
  }

  public async callers(name: string, path: string = "."): Promise<any[]> {
    const raw = await this.callTool("callers", { name, path });
    try {
      return JSON.parse(raw);
    } catch {
      return [];
    }
  }

  public async callees(name: string, path: string = "."): Promise<any[]> {
    const raw = await this.callTool("callees", { name, path });
    try {
      return JSON.parse(raw);
    } catch {
      return [];
    }
  }

  public async eventsSummary(path: string = "."): Promise<string> {
    return await this.callTool("events_summary", { path });
  }

  public async review(file: string, path: string = "."): Promise<string> {
    return await this.callTool("review", { file, path });
  }

  public sendNotification(method: string, params?: any): void {
    if (!this.process?.stdin?.writable) {
      return;
    }
    const req: JsonRpcRequest = {
      jsonrpc: "2.0",
      method,
      params,
    };
    this.process.stdin.write(JSON.stringify(req) + "\n");
  }

  public sendRequest(method: string, params?: any): Promise<any> {
    return new Promise((resolve, reject) => {
      if (!this.process?.stdin?.writable) {
        return reject(new Error("MCP client process not connected"));
      }
      const id = this.nextId++;
      this.pendingRequests.set(id, { resolve, reject });
      const req: JsonRpcRequest = {
        jsonrpc: "2.0",
        id,
        method,
        params,
      };
      this.process.stdin.write(JSON.stringify(req) + "\n");
    });
  }

  public stop(): void {
    if (this.process) {
      this.process.kill();
      this.process = null;
      this.initialized = false;
    }
  }
}
