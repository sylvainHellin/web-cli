# web-cli

Self-contained CLI for LLM-friendly web access. Replaces the `pi-web-access`
extension with a single Rust binary (`web`) driven by the `web` skill in
`~/.claude/skills/web/`.

## Install

```bash
cargo install --path .   # installs `web` to ~/.cargo/bin
```

## Subcommands

| Command | What it does | Providers (primary -> fallback) |
| ------- | ------------ | ------------------------------- |
| `web fetch <url>` | URL to clean markdown | crawl4ai (`crwl`) -> Jina Reader -> Exa contents -> raw GET |
| `web search <query>` | Ranked list of links | Brave -> Exa |
| `web answer <query>` | Sourced answer + citations | Exa `/answer` -> Perplexity Sonar |
| `web crawl <url>` | Crawl a site to markdown | crawl4ai (`crwl`) -> Firecrawl v2 |
| `web crawl <url> --map` | List discovered URLs | Firecrawl v2 (paid) |
| `web crawl <url> --depth N` | Depth-limited crawl | Firecrawl v2 (paid) |
| `web browse <cmd> [args]` | Stateful browser control (passthrough) | pw-browse daemon (bundled) -> chrome-devtools |

`--json` on any command for structured output. See `web <cmd> --help`.

### crawl4ai backbone (key-free default)

`fetch` and `crawl` default to [crawl4ai](https://github.com/unclecode/crawl4ai),
shelling out to its bundled `crwl` CLI for local, key-free extraction via a headless
browser. On any failure (binary absent, browser launch error, bot-block, timeout) the
command falls through to the paid providers automatically.

One-time setup:

```bash
uv tool install crawl4ai   # provides `crwl` at ~/.local/bin/crwl
crawl4ai-setup             # installs the Chromium browser crawl4ai drives
```

Two capabilities stay Firecrawl-only (crawl4ai's CLI cannot do them cheaply):
`--map` (no content-free URL-discovery mode) and `--depth` (its deep-crawl hardcodes
max depth = 3). Passing `--depth` on the default backbone prints a note and uses
Firecrawl. One-shot scripted interaction (login then scrape in a single call) stays
on `crwl -C`; see the `web` skill.

### browse backbone (stateful browser control)

`web browse` forwards its args verbatim to a persistent browser-control daemon
(`start` / `status` / `stop`) that owns a single browser instance; all other
commands (`goto`, `snapshot`, `click`, `fill`, `upload`, ...) talk to it, so cookies
and login state survive across calls. `web browse --help` proxies the backend command
list; known telemetry/update banners are stripped from all output.

The bundled default backend is [`pw-browse/`](pw-browse/), a self-contained pnpm/TS
package vendored in this repo. It runs its own long-lived persistent-Playwright daemon
that we detach explicitly and never close on client disconnect. It exists because
`chrome-devtools-mcp` (the previous backend) kept losing its daemon and Chrome minutes
after spawn on this Mac (suspected environment-level process reaping); `pw-browse`
sidesteps that by owning the browser process lifetime itself. `pw-browse/` is a
standalone package, not part of the Rust build. It also ships `rydoo-batch`, a
deterministic monthly Rydoo runner that shares the Playwright dependency.

Build and link the backend once (`browseBin` in the config then points at the shim):

```bash
cd pw-browse
PNPM_HOME="$HOME/Library/pnpm" pnpm install && pnpm build
pnpm exec playwright install chromium          # once, if not cached
PNPM_HOME="$HOME/Library/pnpm" pnpm link --global   # installs `pw-browse` + `rydoo-batch` shims
```

Runner resolution: `browseBin` config override (binary or `.js` entry point run via
node; set to the `pw-browse` shim by default) -> `chrome-devtools` on `$PATH` ->
`pnpm dlx --package <browsePackage> chrome-devtools`, with `browsePackage` pinned to
`chrome-devtools-mcp@1.6.0` by default. See [`pw-browse/README.md`](pw-browse/README.md)
for the verb list, ref scheme, and the `rydoo-batch` workflow.

## Config

`~/.config/web-cli/config.json` (camelCase keys, mode 600). Env vars
(`EXA_API_KEY`, `FIRECRAWL_API_KEY`, `BRAVE_API_KEY`, `PERPLEXITY_API_KEY`,
`JINA_API_KEY`) override file values.

```json
{
  "exaApiKey": "...",
  "firecrawlApiKey": "fc-...",
  "braveApiKey": "...",
  "perplexityApiKey": "pplx-...",
  "jinaApiKey": "jina_...",
  "defaults": {
    "answerProvider": "exa",
    "perplexityModel": "sonar",
    "searchResults": 8,
    "saveThreshold": 30000,
    "fetchBackbone": "crawl4ai",
    "crawlBackbone": "crawl4ai"
  }
}
```

No keys are required for `fetch`/`crawl` once crawl4ai is installed. Pin
`fetchBackbone` to `"jina"` or `crawlBackbone` to `"firecrawl"` to skip crawl4ai;
set `crawl4aiBin` to override the `crwl` path. Exa alone still covers search + fetch
+ answer; Firecrawl is required only for `--map`, `--depth`, and as crawl fallback.

## Design notes

- One binary: `crwl` subprocess for the crawl4ai backbone, `curl`+JSON over each
  paid provider's REST API. No MCP, no node deps in the binary itself.
- Output over `saveThreshold` chars is written to `$TMPDIR/web-cli/` and the path
  is printed, so an agent reads slices instead of flooding its context.
- Config path honours `$XDG_CONFIG_HOME` then `~/.config` (not the macOS
  Application Support dir the `directories` crate defaults to).
