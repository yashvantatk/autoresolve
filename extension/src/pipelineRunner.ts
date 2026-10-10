import * as cp from "child_process";
import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { AutoResolveEvent, PipelineProgress, Plan } from "./types";

export class PipelineRunner {
  private activeProcess: cp.ChildProcess | null = null;
  private isRunning = false;

  constructor(
    private readonly binaryPath: string,
    private readonly workspaceRoot: string
  ) {}

  public get running(): boolean {
    return this.isRunning;
  }

  public async runFix(
    relativeFilePath: string,
    onProgress?: (p: PipelineProgress) => void,
    onEvent?: (e: AutoResolveEvent) => void
  ): Promise<Plan | null> {
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
      } catch {
        lastBytesRead = 0;
      }
    }

    const args = ["fix", relativeFilePath, "--root", "."];

    return new Promise<Plan | null>((resolve, reject) => {
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

            stream.on("data", (chunk: string | Buffer) => {
              lineBuffer += chunk.toString();
              const lines = lineBuffer.split("\n");
              lineBuffer = lines.pop() || "";

              for (const line of lines) {
                const trimmed = line.trim();
                if (!trimmed) {
                  continue;
                }
                try {
                  const ev: AutoResolveEvent = JSON.parse(trimmed);
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
                  } else if (ev.kind === "outcome") {
                    percent = 95;
                  } else if (ev.kind === "check" || currentRole === "gate") {
                    percent = 90;
                  } else if (currentRole === "fixer") {
                    percent = 80;
                  } else if (currentRole === "guard" || ev.kind === "guard") {
                    percent = 65;
                  } else if (currentRole === "tester" || ev.kind === "repro") {
                    percent = 50;
                  } else if (currentRole === "skeptic") {
                    percent = 35;
                  } else if (currentRole.includes("reviewer") || ev.kind === "review_done") {
                    percent = 20;
                  } else if (ev.kind === "run_start") {
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
                } catch (e) {
                  // Partial JSON line; ignore
                }
              }
            });
          }
        } catch {
          // File stat error; ignore
        }
      };

      const pollInterval = setInterval(pollEvents, 300);

      this.activeProcess.stdout?.on("data", (d: Buffer) => {
        console.log(`[AutoResolve fix stdout]: ${d.toString()}`);
      });

      this.activeProcess.stderr?.on("data", (d: Buffer) => {
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
            const plan: Plan = JSON.parse(rawPlan);
            resolve(plan);
            return;
          } catch (e) {
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

  public cancel(): void {
    if (this.activeProcess) {
      this.activeProcess.kill("SIGINT");
      this.activeProcess = null;
      this.isRunning = false;
    }
  }
}
