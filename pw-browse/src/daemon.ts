// The long-lived daemon. It owns the Chromium process for its entire lifetime.
// It is spawned detached by the client (see cli.ts spawnDaemon) so it survives
// the death of the shell that started it. It only exits on an explicit `stop`
// verb or when the browser process dies.

import { createServer, type Socket } from "node:net";
import { existsSync, mkdirSync, unlinkSync, writeFileSync, appendFileSync } from "node:fs";
import { chromium, type BrowserContext, type Page } from "playwright";
import {
  SOCK_PATH,
  PID_PATH,
  LOG_PATH,
  META_PATH,
  STATE_DIR,
  type Request,
  type Response,
} from "./protocol.js";
import { addExpense } from "./add-expense.js";

function ts(): string {
  return new Date().toISOString();
}

function log(msg: string): void {
  const line = `[${ts()}] ${msg}\n`;
  try {
    appendFileSync(LOG_PATH, line);
  } catch {
    // best effort
  }
}

let context: BrowserContext | null = null;
let page: Page | null = null;
let profileDir = "";
let startedAt = "";

// Ref mechanism (our own, self-contained): snapshot() stamps every interactable
// element with a `data-pwref="eNN"` attribute inside the page, then emits an
// accessibility-flavored tree keyed by those refs. resolveRef() turns a ref back
// into a Playwright Locator via the `[data-pwref="..."]` CSS attribute selector.
// Refs are stable for as long as the DOM node lives; a fresh snapshot re-stamps
// and reuses ids where possible. We chose this over Playwright's internal
// aria-ref engine because `_snapshotForAI` is not exposed on the public Node page
// object in playwright 1.61.1 (it lives in the MCP/CDP layer).
function resolveRef(ref: string) {
  if (!page) throw new Error("no page");
  // Escape hatches: explicit css= prefix, or anything that looks like a selector.
  if (ref.startsWith("css=")) return page.locator(ref);
  if (/^e\d+$/.test(ref)) return page.locator(`[data-pwref="${ref}"]`);
  // Treat anything else (contains a dot, #, [, space, etc.) as a CSS selector.
  return page.locator(ref);
}

// Injected into the page. Walks the DOM, tags interactable / labelled elements
// with data-pwref, and returns an indented text tree of role + name + [ref].
const SNAPSHOT_FN = `() => {
  const INTERACTIVE = new Set(['a','button','input','select','textarea','summary','option']);
  let counter = 0;
  const roleOf = (el) => {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === 'a' && el.hasAttribute('href')) return 'link';
    if (tag === 'button') return 'button';
    if (tag === 'input') {
      const t = (el.getAttribute('type') || 'text').toLowerCase();
      if (t === 'checkbox') return 'checkbox';
      if (t === 'radio') return 'radio';
      if (t === 'button' || t === 'submit' || t === 'reset') return 'button';
      if (t === 'file') return 'file-input';
      return 'textbox';
    }
    if (tag === 'textarea') return 'textbox';
    if (tag === 'select') return 'combobox';
    if (tag === 'option') return 'option';
    return tag;
  };
  const nameOf = (el) => {
    const aria = el.getAttribute('aria-label');
    if (aria) return aria.trim();
    if (el.id) {
      const lbl = document.querySelector('label[for="' + CSS.escape(el.id) + '"]');
      if (lbl && lbl.textContent) return lbl.textContent.trim();
    }
    const ph = el.getAttribute('placeholder');
    if (ph) return ph.trim();
    if (el.tagName.toLowerCase() === 'input') {
      const v = el.value;
      if (v) return String(v).slice(0, 60);
    }
    const txt = (el.textContent || '').trim().replace(/\\s+/g, ' ');
    return txt.slice(0, 80);
  };
  const isInteractive = (el) => {
    const tag = el.tagName.toLowerCase();
    if (INTERACTIVE.has(tag)) return true;
    if (el.hasAttribute('role')) return true;
    if (el.hasAttribute('tabindex')) return true;
    if (el.hasAttribute('onclick')) return true;
    return false;
  };
  const lines = [];
  const walk = (el, depth) => {
    let selfLine = null;
    if (el.nodeType === 1 && isInteractive(el)) {
      let ref = el.getAttribute('data-pwref');
      if (!ref) { ref = 'e' + (++counter); el.setAttribute('data-pwref', ref); }
      else { const n = parseInt(ref.slice(1), 10); if (n > counter) counter = n; }
      const hidden = el.type === 'hidden' || (el.offsetParent === null && getComputedStyle(el).display === 'none');
      const flags = [];
      if (hidden) flags.push('hidden');
      if (el.disabled) flags.push('disabled');
      selfLine = '  '.repeat(depth) + roleOf(el) + ' "' + nameOf(el) + '" [' + ref + ']' + (flags.length ? ' (' + flags.join(',') + ')' : '');
      lines.push(selfLine);
    }
    const nextDepth = selfLine ? depth + 1 : depth;
    for (const child of el.children) walk(child, nextDepth);
  };
  walk(document.body, 0);
  return lines.join('\\n');
}`;

async function snapshot(): Promise<string> {
  if (!page) throw new Error("browser not started");
  // Evaluate the snapshot source inside the page. We pass the function source as a
  // string and reconstruct it in-page so nothing depends on Node-side closures.
  return (await page.evaluate((src: string) => {
    // eslint-disable-next-line no-new-func
    const fn = new Function(`return (${src});`)();
    return fn();
  }, SNAPSHOT_FN)) as string;
}

async function handle(req: Request): Promise<Response> {
  const { verb, args } = req;
  try {
    switch (verb) {
      case "status": {
        return {
          ok: true,
          data: {
            running: !!context,
            profile: profileDir,
            startedAt,
            url: page ? page.url() : null,
            pid: process.pid,
          },
        };
      }
      case "goto": {
        if (!page) throw new Error("browser not started");
        const url = String(args.url ?? "");
        if (!url) throw new Error("goto requires --url / <url>");
        await page.goto(url, { waitUntil: "domcontentloaded", timeout: 30000 });
        return { ok: true, data: { url: page.url() } };
      }
      case "snapshot": {
        const snap = await snapshot();
        return { ok: true, data: { snapshot: snap } };
      }
      case "click": {
        const ref = String(args.ref ?? "");
        if (!ref) throw new Error("click requires <ref>");
        await resolveRef(ref).click({ timeout: 15000 });
        return { ok: true, data: { clicked: ref } };
      }
      case "fill": {
        const ref = String(args.ref ?? "");
        const text = String(args.text ?? "");
        if (!ref) throw new Error("fill requires <ref> <text>");
        await resolveRef(ref).fill(text, { timeout: 15000 });
        return { ok: true, data: { filled: ref } };
      }
      case "press": {
        const ref = String(args.ref ?? "");
        const key = String(args.key ?? "");
        if (!ref || !key) throw new Error("press requires <ref> <key>");
        await resolveRef(ref).press(key, { timeout: 15000 });
        return { ok: true, data: { pressed: key, ref } };
      }
      case "upload": {
        // Sets files directly on an <input type=file> via setInputFiles. Never
        // opens the native OS picker, and works on hidden inputs.
        const ref = String(args.ref ?? "");
        const file = String(args.file ?? "");
        if (!ref || !file) throw new Error("upload requires <ref|css> <file>");
        if (!existsSync(file)) throw new Error(`upload file not found: ${file}`);
        await resolveRef(ref).setInputFiles(file, { timeout: 15000 });
        return { ok: true, data: { uploaded: file, ref } };
      }
      case "eval": {
        if (!page) throw new Error("browser not started");
        const js = String(args.js ?? "");
        if (!js) throw new Error("eval requires <js>");
        // Evaluate as a function body so bare expressions and statements both work.
        const result = await page.evaluate(
          (code) => {
            // eslint-disable-next-line no-new-func
            const fn = new Function(`return (${code});`);
            try {
              return fn();
            } catch {
              const fn2 = new Function(code);
              return fn2();
            }
          },
          js,
        );
        return { ok: true, data: { result } };
      }
      case "add-expense": {
        if (!page) throw new Error("browser not started");
        const spec = args.spec;
        if (!spec || typeof spec !== "object") throw new Error("add-expense requires --spec <path.json> (parsed object)");
        const dryRun = args.dryRun === true;
        const res = await addExpense(page, spec, dryRun);
        return { ok: true, data: res };
      }
      case "stop": {
        return { ok: true, data: { stopping: true } };
      }
      default:
        throw new Error(`unknown verb: ${verb}`);
    }
  } catch (err) {
    // Collapse multi-line Playwright errors (call logs) to a single clean line.
    const raw = (err as Error).message || String(err);
    const oneLine = raw.split("\n")[0].trim();
    return { ok: false, error: oneLine };
  }
}

// Guard against reentrancy: `context.close()` fires the context "close" event,
// whose handler calls shutdown again. The second call used to process.exit()
// while the first close was still flushing the profile, which let Playwright's
// exit reaper SIGKILL Chromium and set its crash-restore flag ("Restore pages?").
let shuttingDown = false;

async function shutdown(reason: string): Promise<void> {
  if (shuttingDown) return;
  shuttingDown = true;
  log(`shutdown: ${reason}`);
  try {
    if (context) await context.close();
  } catch {
    // ignore
  }
  try {
    if (existsSync(SOCK_PATH)) unlinkSync(SOCK_PATH);
  } catch {
    // ignore
  }
  try {
    if (existsSync(PID_PATH)) unlinkSync(PID_PATH);
  } catch {
    // ignore
  }
  process.exit(0);
}

async function main(): Promise<void> {
  if (!existsSync(STATE_DIR)) mkdirSync(STATE_DIR, { recursive: true });

  profileDir = process.env.PW_BROWSE_PROFILE || "";
  if (!profileDir) {
    log("fatal: PW_BROWSE_PROFILE not set");
    process.exit(1);
  }
  const initialUrl = process.env.PW_BROWSE_URL || "";
  const headless = process.env.PW_BROWSE_HEADLESS === "1";

  // Do not read stdin. Detach from any controlling terminal influence and ignore
  // SIGHUP so closing the launching shell cannot take us down.
  try {
    process.stdin.destroy();
  } catch {
    // ignore
  }
  process.on("SIGHUP", () => log("ignored SIGHUP"));
  process.on("SIGPIPE", () => log("ignored SIGPIPE"));

  log(`daemon starting pid=${process.pid} profile=${profileDir} headless=${headless}`);

  context = await chromium.launchPersistentContext(profileDir, {
    headless,
    // Keep the browser awake and stable on macOS: disable background throttling
    // and backgrounding so a headed window that gets no input is not suspended
    // (App Nap / renderer throttling analog for the Chromium side).
    args: [
      "--disable-background-timer-throttling",
      "--disable-backgrounding-occluded-windows",
      "--disable-renderer-backgrounding",
      "--disable-features=CalculateNativeWinOcclusion",
    ],
    viewport: null,
  });

  page = context.pages()[0] ?? (await context.newPage());
  startedAt = ts();

  if (initialUrl) {
    try {
      await page.goto(initialUrl, { waitUntil: "domcontentloaded", timeout: 30000 });
    } catch (e) {
      log(`initial goto failed: ${(e as Error).message}`);
    }
  }

  // If the browser process dies for any reason, the daemon has no purpose. Exit.
  // (No-op when a graceful `stop` shutdown is already in progress.)
  context.on("close", () => {
    void shutdown("browser context closed / browser process died");
  });

  writeFileSync(META_PATH, JSON.stringify({ pid: process.pid, profileDir, startedAt }));
  writeFileSync(PID_PATH, String(process.pid));

  if (existsSync(SOCK_PATH)) {
    try {
      unlinkSync(SOCK_PATH);
    } catch {
      // ignore
    }
  }

  const server = createServer((sock: Socket) => {
    let buf = "";
    sock.on("data", async (chunk) => {
      buf += chunk.toString("utf8");
      let idx: number;
      while ((idx = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, idx);
        buf = buf.slice(idx + 1);
        if (!line.trim()) continue;
        let req: Request;
        try {
          req = JSON.parse(line) as Request;
        } catch {
          sock.write(JSON.stringify({ ok: false, error: "bad request json" }) + "\n");
          continue;
        }
        const res = await handle(req);
        sock.write(JSON.stringify(res) + "\n");
        if (req.verb === "stop" && res.ok) {
          // Flush then shut down. Never triggered by disconnect, only this verb.
          sock.end();
          void shutdown("stop verb received");
        }
      }
    });
    // Client disconnect must never affect the browser. Just log at debug level.
    sock.on("error", () => {
      /* client vanished mid-write; ignore */
    });
  });

  server.on("error", (e) => log(`server error: ${(e as Error).message}`));
  server.listen(SOCK_PATH, () => log(`listening on ${SOCK_PATH}`));

  process.on("uncaughtException", (e) => log(`uncaughtException: ${e.stack || e}`));
  process.on("unhandledRejection", (e) => log(`unhandledRejection: ${String(e)}`));
}

void main();
