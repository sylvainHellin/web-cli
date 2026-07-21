#!/usr/bin/env node
// Thin client CLI. Same binary as the daemon runner. Each invocation connects to
// the running daemon over the unix socket, sends one verb, prints the result, and
// exits. `start` is special: it spawns the daemon detached and waits for it to
// come up.

import { connect } from "node:net";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import {
  SOCK_PATH,
  PID_PATH,
  LOG_PATH,
  STATE_DIR,
  type Request,
  type Response,
} from "./protocol.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

function die(msg: string): never {
  process.stderr.write(msg.replace(/\n+$/, "") + "\n");
  process.exit(1);
}

// Send one request to the daemon over the socket and resolve its response.
function sendOnce(req: Request, timeoutMs = 60000): Promise<Response> {
  return new Promise((resolvePromise, reject) => {
    const sock = connect(SOCK_PATH);
    let buf = "";
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error("daemon request timed out"));
    }, timeoutMs);
    sock.on("connect", () => {
      sock.write(JSON.stringify(req) + "\n");
    });
    sock.on("data", (chunk) => {
      buf += chunk.toString("utf8");
      const idx = buf.indexOf("\n");
      if (idx >= 0) {
        clearTimeout(timer);
        const line = buf.slice(0, idx);
        sock.end();
        try {
          resolvePromise(JSON.parse(line) as Response);
        } catch {
          reject(new Error("bad response json from daemon"));
        }
      }
    });
    sock.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
  });
}

function daemonAlive(): boolean {
  if (!existsSync(PID_PATH)) return false;
  try {
    const pid = parseInt(readFileSync(PID_PATH, "utf8").trim(), 10);
    if (!pid) return false;
    process.kill(pid, 0); // throws if not alive
    return true;
  } catch {
    return false;
  }
}

async function waitForDaemon(timeoutMs = 45000): Promise<Response> {
  const deadline = Date.now() + timeoutMs;
  let lastErr = "";
  while (Date.now() < deadline) {
    try {
      return await sendOnce({ verb: "status", args: {} }, 3000);
    } catch (e) {
      lastErr = (e as Error).message;
      await new Promise((r) => setTimeout(r, 500));
    }
  }
  throw new Error(`daemon did not come up: ${lastErr}`);
}

// Spawn the daemon fully detached so it outlives this process and its shell.
function spawnDaemon(profile: string, url: string, headless: boolean): void {
  if (!existsSync(STATE_DIR)) mkdirSync(STATE_DIR, { recursive: true });
  const daemonEntry = join(__dirname, "daemon.js");
  if (!existsSync(daemonEntry)) die(`daemon entry missing: ${daemonEntry} (run pnpm build)`);

  // Redirect all stdio to the log file; never inherit stdin.
  const out = openSync(LOG_PATH, "a");
  const child = spawn(process.execPath, [daemonEntry], {
    detached: true,
    stdio: ["ignore", out, out],
    env: {
      ...process.env,
      PW_BROWSE_PROFILE: resolve(profile),
      PW_BROWSE_URL: url,
      PW_BROWSE_HEADLESS: headless ? "1" : "0",
    },
  });
  // Detach the child from this process's lifetime: new session leader (detached)
  // plus unref so the parent event loop can exit immediately.
  child.unref();
  writeFileSync(join(STATE_DIR, "spawn.pid"), String(child.pid ?? ""));
}

// Minimal positional + flag parser. Flags: --key value or --key (boolean).
function parseArgs(argv: string[]): { pos: string[]; flags: Record<string, string | boolean> } {
  const pos: string[] = [];
  const flags: Record<string, string | boolean> = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const key = a.slice(2);
      const next = argv[i + 1];
      if (next === undefined || next.startsWith("--")) {
        flags[key] = true;
      } else {
        flags[key] = next;
        i++;
      }
    } else {
      pos.push(a);
    }
  }
  return { pos, flags };
}

function print(res: Response, snapshotFile?: string): void {
  if (!res.ok) die(res.error || "unknown error");
  const data = res.data as Record<string, unknown> | undefined;
  if (data && typeof data.snapshot === "string") {
    if (snapshotFile) {
      writeFileSync(snapshotFile, data.snapshot);
      process.stdout.write(`snapshot written to ${snapshotFile}\n`);
    } else {
      process.stdout.write(data.snapshot + "\n");
    }
    return;
  }
  process.stdout.write(JSON.stringify(res.data) + "\n");
}

async function main(): Promise<void> {
  const [verb, ...rest] = process.argv.slice(2);
  const { pos, flags } = parseArgs(rest);

  if (!verb || verb === "help" || verb === "--help") {
    process.stdout.write(
      [
        "pw-browse - persistent Playwright browser CLI",
        "",
        "  start --profile <dir> [--url <u>] [--headless]",
        "  status",
        "  stop",
        "  goto <url>",
        "  snapshot [--file <path>]",
        "  click <ref>",
        "  fill <ref> <text>",
        "  press <ref> <key>",
        "  upload <ref|css> <file>",
        "  eval <js>",
        "  add-expense --spec <path.json> [--dry-run]",
      ].join("\n") + "\n",
    );
    return;
  }

  if (verb === "start") {
    const profile = (flags.profile as string) || "";
    if (!profile) die("start requires --profile <dir>");
    const url = (flags.url as string) || "";
    const headless = flags.headless === true;
    if (daemonAlive()) {
      const st = await sendOnce({ verb: "status", args: {} }).catch(() => null);
      if (st?.ok) {
        print(st);
        return;
      }
    }
    spawnDaemon(profile, url, headless);
    const st = await waitForDaemon();
    print(st);
    return;
  }

  // All other verbs require a live daemon.
  if (!daemonAlive()) die("daemon not running (use: pw-browse start --profile <dir>)");

  let req: Request;
  switch (verb) {
    case "status":
      req = { verb, args: {} };
      break;
    case "stop":
      req = { verb, args: {} };
      break;
    case "goto":
      req = { verb, args: { url: (flags.url as string) || pos[0] } };
      break;
    case "snapshot":
      req = { verb, args: {} };
      break;
    case "click":
      req = { verb, args: { ref: pos[0] } };
      break;
    case "fill":
      req = { verb, args: { ref: pos[0], text: pos.slice(1).join(" ") } };
      break;
    case "press":
      req = { verb, args: { ref: pos[0], key: pos[1] } };
      break;
    case "upload":
      req = { verb, args: { ref: pos[0], file: pos[1] } };
      break;
    case "eval":
      req = { verb, args: { js: pos.join(" ") } };
      break;
    case "add-expense": {
      const specPath = (flags.spec as string) || pos[0];
      if (!specPath) die("add-expense requires --spec <path.json>");
      if (!existsSync(specPath)) die(`spec file not found: ${specPath}`);
      let spec: unknown;
      try {
        spec = JSON.parse(readFileSync(specPath, "utf8"));
      } catch (e) {
        die(`bad spec json: ${(e as Error).message}`);
      }
      const dryRun = flags["dry-run"] === true || flags.dryRun === true;
      req = { verb: "add-expense", args: { spec, dryRun } };
      break;
    }
    default:
      die(`unknown verb: ${verb}`);
  }

  try {
    // add-expense drives a long multi-step form (uploads, async dropdowns, EUR
    // override with a row reopen); give it a generous timeout.
    const timeoutMs = verb === "add-expense" ? 300000 : 60000;
    const res = await sendOnce(req, timeoutMs);
    print(res, flags.file as string | undefined);
  } catch (e) {
    die((e as Error).message);
  }
}

void main();
