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
exports.PipelineRunner = void 0;
const cp = __importStar(require("child_process"));
const fs = __importStar(require("fs"));
const path = __importStar(require("path"));
class PipelineRunner {
    binaryPath;
    workspaceRoot;
    activeProcess = null;
    isRunning = false;
    constructor(binaryPath, workspaceRoot) {
        this.binaryPath = binaryPath;
        this.workspaceRoot = workspaceRoot;
    }
    get running() {
        return this.isRunning;
    }
    async runFix(relativeFilePath, onProgress, onEvent) {
        if (this.isRunning) {
            throw new Error("An AutoResolve run is already in progress.");
        }
        this.isRunning = true;
        const eventsPath = path.join(this.workspaceRoot, ".autoresolve", "events.jsonl");
        const planPath = path.join(this.workspaceRoot, ".autoresolve", "plan.json");
        let lastBytesRead = 0;
        if (fs.existsSync(eventsPath)) {
            try {
                lastBytesRead = fs.statSync(eventsPath).size;
            }
            catch {
                lastBytesRead = 0;
            }
        }
        const args = ["fix", relativeFilePath, "--root", "."];
        return new Promise((resolve, reject) => {
            this.activeProcess = cp.spawn(this.binaryPath, args, {
                cwd: this.workspaceRoot,
                env: {
                    ...process.env,
                },
            });
            let lineBuffer = "";
            let eventsCount = 0;
            let confirmedIssues = 0;
            let provenFixes = 0;
            let currentRole = "controller";
            let currentKind = "starting";
            let currentTitle = "";
            const pollEvents = () => {
                if (!fs.existsSync(eventsPath)) {
                    return;
                }
                try {
                    const stat = fs.statSync(eventsPath);
                    if (stat.size > lastBytesRead) {
                        const stream = fs.createReadStream(eventsPath, {
                            start: lastBytesRead,
                            end: stat.size,
                            encoding: "utf8",
                        });
                        lastBytesRead = stat.size;
                        stream.on("data", (chunk) => {
                            lineBuffer += chunk.toString();
                            const lines = lineBuffer.split("\n");
                            lineBuffer = lines.pop() || "";
                            for (const line of lines) {
                                const trimmed = line.trim();
                                if (!trimmed) {
                                    continue;
                                }
                                try {
                                    const ev = JSON.parse(trimmed);
                                    eventsCount++;
                                    currentRole = ev.role || currentRole;
                                    currentKind = ev.kind || currentKind;
                                    if (ev.kind === "issue_start" && ev.data?.title) {
                                        currentTitle = ev.data.title;
                                    }
                                    if (ev.kind === "review_done" && typeof ev.data?.confirmed === "number") {
                                        confirmedIssues = ev.data.confirmed;
                                    }
                                    if (ev.kind === "outcome" && ev.data?.proven) {
                                        provenFixes++;
                                    }
                                    let percent = 10;
                                    let detail = `${currentRole.toUpperCase()}: ${currentKind}`;
                                    if (ev.kind === "run_end") {
                                        percent = 100;
                                        currentRole = "done";
                                        detail = "Complete";
                                    }
                                    else if (ev.kind === "outcome") {
                                        percent = 95;
                                    }
                                    else if (ev.kind === "check" || currentRole === "gate") {
                                        percent = 90;
                                    }
                                    else if (currentRole === "fixer") {
                                        percent = 80;
                                    }
                                    else if (currentRole === "guard" || ev.kind === "guard") {
                                        percent = 65;
                                    }
                                    else if (currentRole === "tester" || ev.kind === "repro") {
                                        percent = 50;
                                    }
                                    else if (currentRole === "skeptic") {
                                        percent = 35;
                                    }
                                    else if (currentRole.includes("reviewer") || ev.kind === "review_done") {
                                        percent = 20;
                                    }
                                    else if (ev.kind === "run_start") {
                                        percent = 5;
                                    }
                                    if (ev.kind !== "run_end" && currentTitle) {
                                        detail += ` (${currentTitle})`;
                                    }
                                    onEvent?.(ev);
                                    onProgress?.({
                                        runId: ev.run,
                                        currentRole,
                                        currentKind,
                                        currentTitle,
                                        percent,
                                        eventsCount,
                                        confirmedIssues,
                                        provenFixes,
                                        detail,
                                    });
                                }
                                catch (e) {
                                    // Partial JSON line; ignore
                                }
                            }
                        });
                    }
                }
                catch {
                    // File stat error; ignore
                }
            };
            const pollInterval = setInterval(pollEvents, 300);
            this.activeProcess.stdout?.on("data", (d) => {
                console.log(`[AutoResolve fix stdout]: ${d.toString()}`);
            });
            this.activeProcess.stderr?.on("data", (d) => {
                console.warn(`[AutoResolve fix stderr]: ${d.toString()}`);
            });
            this.activeProcess.on("exit", (code) => {
                clearInterval(pollInterval);
                pollEvents();
                this.isRunning = false;
                this.activeProcess = null;
                onProgress?.({
                    runId: "",
                    currentRole: "done",
                    currentKind: "run_end",
                    percent: 100,
                    eventsCount,
                    confirmedIssues,
                    provenFixes,
                    detail: "Complete",
                });
                if (fs.existsSync(planPath)) {
                    try {
                        const rawPlan = fs.readFileSync(planPath, "utf8");
                        const plan = JSON.parse(rawPlan);
                        resolve(plan);
                        return;
                    }
                    catch (e) {
                        console.error("Failed to parse plan.json", e);
                    }
                }
                resolve(null);
            });
            this.activeProcess.on("error", (err) => {
                clearInterval(pollInterval);
                this.isRunning = false;
                this.activeProcess = null;
                reject(err);
            });
        });
    }
    cancel() {
        if (this.activeProcess) {
            this.activeProcess.kill("SIGINT");
            this.activeProcess = null;
            this.isRunning = false;
        }
    }
}
exports.PipelineRunner = PipelineRunner;
//# sourceMappingURL=pipelineRunner.js.map