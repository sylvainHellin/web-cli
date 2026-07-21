# pw-browse

A minimal DIY persistent-Playwright browser-automation CLI.
The whole point is that **we** own the browser process lifetime.

Two packaged tools (`chrome-devtools-mcp`, `playwright-cli`) kept losing their daemon and Chrome minutes after spawn on this Mac (clean exits, suspected environment-level process reaping).
`pw-browse` sidesteps that by running its own long-lived daemon that we detach explicitly and never close on client disconnect.

## Architecture

One long-lived **daemon** process launches Chromium via `chromium.launchPersistentContext(profileDir, { headless: false })` and serves verbs over a unix domain socket (NDJSON).
A thin **client** (same binary, subcommands) connects once per call, sends one verb, prints the result, and exits.

- The daemon is spawned `detached` with `stdio` redirected to a log file and stdin ignored, then `unref`-ed, so it survives the death of the launching shell (it reparents to pid 1, becomes a session leader).
- It ignores `SIGHUP` and `SIGPIPE`.
- Chromium is launched with background-throttling and occlusion features disabled so a headed window that receives no input is not suspended.
- The daemon closes the browser and exits **only** on an explicit `stop` verb or when the browser process dies (logged with a timestamp). Client disconnect never touches the browser.

State lives in `~/.pw-browse/`:

| file          | purpose                              |
| ------------- | ------------------------------------ |
| `daemon.sock` | unix socket the client connects to   |
| `daemon.pid`  | daemon pid (liveness check)          |
| `daemon.log`  | timestamped daemon log               |
| `daemon.json` | metadata (pid, profile, startedAt)   |

## Build

```sh
pnpm install
pnpm build          # tsc -> dist/
pnpm exec playwright install chromium   # once, if not cached
```

## Launch recipe

```sh
# start the persistent daemon + headed browser (detaches immediately)
node dist/cli.js start --profile /path/to/profile-dir --url https://example.com

# from then on, every call is a one-shot client connection:
node dist/cli.js status
node dist/cli.js goto https://example.com
node dist/cli.js snapshot
node dist/cli.js stop
```

The daemon keeps running after `start` returns and after the shell exits.
To reach it later, just run any verb from any shell; it finds the socket in `~/.pw-browse/`.

## Verbs

| verb                        | description                                                                 |
| --------------------------- | --------------------------------------------------------------------------- |
| `start --profile <dir> [--url <u>] [--headless]` | spawn the detached daemon and headed browser; idempotent if already running |
| `status`                    | print running state, profile, current url, pid                              |
| `stop`                      | close browser and exit the daemon                                           |
| `goto <url>`                | navigate current page (waits for domcontentloaded)                          |
| `snapshot [--file <path>]`  | accessibility-flavored tree with stable element refs; stdout or `--file`    |
| `click <ref>`               | click the element for `<ref>`                                               |
| `fill <ref> <text>`         | fill a textbox                                                              |
| `press <ref> <key>`         | send a key (e.g. `Enter`) to an element                                     |
| `upload <ref\|css> <file>`  | `setInputFiles` on an `<input type=file>`; never opens the native picker, works on hidden inputs |
| `eval <js>`                 | evaluate JS in the page; returns the (serializable) result                  |

Errors are one-line messages on stderr with a nonzero exit code.

## Refs (the snapshot -> action mechanism)

We use our own self-contained ref scheme rather than Playwright's internal
`aria-ref` / `_snapshotForAI`, because `_snapshotForAI` is **not** exposed on the
public Node `Page` object in playwright 1.61.1 (it lives in the MCP/CDP layer, and
probing it on a `launchPersistentContext` page returned `undefined`).

`snapshot` injects JS that walks the DOM, stamps every interactable / labelled
element with a `data-pwref="eNN"` attribute, and emits an indented `role "name" [ref]`
tree (hidden and disabled elements are flagged).
`click`/`fill`/`press`/`upload` resolve a ref back to a locator via the
`[data-pwref="eNN"]` CSS attribute selector.

Refs are stable for as long as the DOM node lives; a fresh snapshot re-stamps and
reuses ids where the attribute is still present.
Ref arguments also accept a raw CSS selector (anything that is not `eNN`), or an
explicit `css=` prefix, as an escape hatch.

Typical flow: `snapshot` to read refs, then act within the same page state.

## Monthly Rydoo run (`rydoo-batch`)

`rydoo-batch` is a standalone deterministic script for the recurring monthly Rydoo draft-expense task.
It is the production runner: **zero LLM calls at runtime**.
The agent's job is only to author and repair the selectors, never to drive the UI on every run ("script drives, agent repairs").

It does NOT use the daemon: it owns its own browser via `chromium.launchPersistentContext(~/.cache/pw-browse-rydoo, { headless: false })` and closes the context on exit.

```sh
pnpm build
# validation mode: fill every field, upload the invoice, assert readbacks, then Cancel.
# Nothing is saved; the script asserts the list row count is unchanged.
node dist/rydoo-batch.js --spec examples/entries-template.json --dry-run
# only the nth (1-based) entry:
node dist/rydoo-batch.js --spec entries.json --dry-run --entry 1
# live: creates drafts (still never clicks Submit):
node dist/rydoo-batch.js --spec entries.json
```

Flags: `--spec <path>` (required), `--dry-run`, `--entry <n>`, `--profile <dir>` (default `~/.cache/pw-browse-rydoo`), `--email <addr>` (SSO email for the silent login bounce, default `sylvain.hellin@hines.com`), `--foreground`.

### Unobtrusive launch (window minimized by default)

Each run owns a headed Chromium, and `launchPersistentContext` normally brings that window to the front and captures keyboard focus, so a monthly run would pop in front of whatever you are typing.
By default the script minimizes the window immediately after launch over CDP (`Browser.getWindowForTarget` + `Browser.setWindowBounds { windowState: "minimized" }`), so it never steals focus.
The anti-throttle Chromium flags (`--disable-background-timer-throttling`, `--disable-backgrounding-occluded-windows`, `--disable-renderer-backgrounding`, `--disable-features=CalculateNativeWinOcclusion`) keep the minimized window fully responsive, so the form drives normally while hidden.
Pass `--foreground` to keep the window visible; use it for the manual MFA handoff below, where you must interact with the Microsoft login page yourself.

### Prepare the spec

The spec is a JSON array of entries. Copy `examples/entries-template.json` (prefilled with the recurring merchants Render, Anthropic Max, Google Cloud, DigitalOcean, OpenAI) and each month update `amount`, `date`, `eurOverride`, `invoicePath`, and `bankProofPath`.

Per-entry fields:

| field | meaning |
| --- | --- |
| `amount` | native amount, decimal comma ok (`"14"`, `"109,50"`) |
| `currency` | `"EUR"` or `"USD"` |
| `merchant`, `date` (`dd/mm/yyyy`), `category` | plain text |
| `projectFilter` / `projectLabel` | text typed to filter the project dropdown, and the exact option label to pick + assert |
| `departmentPreference` | acceptable options in preference order, e.g. `["N/A","Development RBG"]`; the first present option is chosen |
| `investor` | exact option label, or `null` to leave empty |
| `location`, `comment` | plain text |
| `invoicePath` | receipt PDF (`~` expands, relative resolved to cwd) |
| `eurOverride` | converted EUR amount (decimal comma) for USD entries, or `null` |
| `bankProofPath` | ING/Finom statement PDF for USD entries, or `null` |

### End-to-end monthly workflow

1. Update the spec from the month's invoices and ING statements.
2. Dry-run: `node dist/rydoo-batch.js --spec entries.json --dry-run`. Every fill + readback assertion must pass and each entry must Cancel with the list count unchanged.
3. Live: `node dist/rydoo-batch.js --spec entries.json`. This stages the drafts for your existing human review inside Rydoo. It never submits.
4. Review + submit the drafts manually in the Rydoo UI.

### Login: the silent SSO bounce, and when the Entra session lapses

Rydoo's own session cookie does not survive a browser restart, so every fresh start on the persistent profile lands on the `accounts.rydoo.com` login page even while the Microsoft Entra cookie is still alive.
The script handles this itself: it fills the email (`--email`, default `sylvain.hellin@hines.com`), clicks Next, and Entra completes SSO silently with zero MFA interaction, landing back on the expenses list in about ten seconds.
It never types a password and never touches an MFA prompt.

Manual intervention is therefore only needed when the Entra session itself lapses.
A monthly cadence sits inside Entra's 90-day sliding session window, so that should be rare (Conditional Access sign-in-frequency, password change, "sign out of all devices").
When it happens, the silent bounce parks on an interactive Microsoft page instead of reaching the list; the run then prints a `LOGIN REQUIRED` handoff and exits 2 without touching the password or MFA form.
Recovery is a one-time manual headed login:

1. Open the same profile headed and complete Microsoft SSO + MFA once, ticking "stay signed in" so the Entra cookie is persistent. Run the script once with `--foreground` so the window stays visible (e.g. `node dist/rydoo-batch.js --spec entries.json --dry-run --foreground`); log in there, then close it and re-run without `--foreground`.
2. Re-run `rydoo-batch`. Never automate the MFA challenge.

### When a selector breaks

Rydoo occasionally changes its UI and an assertion will fail loudly, naming the entry and step (e.g. `entry 3 (Anthropic) @ readback:project: ...`).
That is the signal to bring the agent back: point Claude + Playwright MCP (or `pw-browse snapshot`) at the live DOM, regenerate the one broken selector in `src/rydoo-batch.ts`, rebuild, and re-run the dry-run.
This is a bounded, diffable selector fix, not a per-run reasoning spend.

## Wiring behind `web browse` (later)

This tool is meant to be pointed at by the existing `web` CLI via its `browseBin`
config so `web browse` uses a stable local daemon instead of a packaged tool.
That wiring is out of scope here and lives in the `web-cli` repo.
