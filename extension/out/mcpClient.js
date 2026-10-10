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
exports.McpClient = void 0;
const cp = __importStar(require("child_process"));
const readline = __importStar(require("readline"));
class McpClient {
    binaryPath;
    workspaceRoot;
    process = null;
    pendingRequests = new Map();
    nextId = 1;
    initialized = false;
    constructor(binaryPath, workspaceRoot) {
        this.binaryPath = binaryPath;
        this.workspaceRoot = workspaceRoot;
    }
    async start() {
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
        rl.on("line", (line) => {
            const trimmed = line.trim();
            if (!trimmed) {
                return;
            }
            try {
                const res = JSON.parse(trimmed);
                if (res.id !== undefined && this.pendingRequests.has(res.id)) {
                    const { resolve, reject } = this.pendingRequests.get(res.id);
                    this.pendingRequests.delete(res.id);
                    if (res.error) {
                        reject(new Error(`MCP error ${res.error.code}: ${res.error.message}`));
                    }
                    else {
                        resolve(res.result);
                    }
                }
            }
            catch (err) {
                console.error("[MCP] Malformed JSON received from server:", line, err);
            }
        });
        this.process.stderr?.on("data", (data) => {
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
    async initialize() {
        const initRes = await this.sendRequest("initialize", {
            protocolVersion: "2025-06-18",
        });
        this.sendNotification("notifications/initialized");
        this.initialized = true;
    }
    async callTool(name, args = {}) {
        if (!this.initialized) {
            await this.start();
        }
        const res = await this.sendRequest("tools/call", {
            name,
            arguments: args,
        });
        const textContent = res?.content?.find((c) => c.type === "text")?.text;
        if (res?.isError) {
            throw new Error(textContent || `Tool ${name} failed`);
        }
        return textContent;
    }
    async scan(path = ".") {
        const raw = await this.callTool("scan", { path });
        try {
            return JSON.parse(raw);
        }
        catch {
            return [];
        }
    }
    async symbols(path = ".") {
        const raw = await this.callTool("symbols", { path });
        try {
            return JSON.parse(raw);
        }
        catch {
            return [];
        }
    }
    async callers(name, path = ".") {
        const raw = await this.callTool("callers", { name, path });
        try {
            return JSON.parse(raw);
        }
        catch {
            return [];
        }
    }
    async callees(name, path = ".") {
        const raw = await this.callTool("callees", { name, path });
        try {
            return JSON.parse(raw);
        }
        catch {
            return [];
        }
    }
    async eventsSummary(path = ".") {
        return await this.callTool("events_summary", { path });
    }
    async review(file, path = ".") {
        return await this.callTool("review", { file, path });
    }
    sendNotification(method, params) {
        if (!this.process?.stdin?.writable) {
            return;
        }
        const req = {
            jsonrpc: "2.0",
            method,
            params,
        };
        this.process.stdin.write(JSON.stringify(req) + "\n");
    }
    sendRequest(method, params) {
        return new Promise((resolve, reject) => {
            if (!this.process?.stdin?.writable) {
                return reject(new Error("MCP client process not connected"));
            }
            const id = this.nextId++;
            this.pendingRequests.set(id, { resolve, reject });
            const req = {
                jsonrpc: "2.0",
                id,
                method,
                params,
            };
            this.process.stdin.write(JSON.stringify(req) + "\n");
        });
    }
    stop() {
        if (this.process) {
            this.process.kill();
            this.process = null;
            this.initialized = false;
        }
    }
}
exports.McpClient = McpClient;
//# sourceMappingURL=mcpClient.js.map