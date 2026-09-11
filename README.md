# web-cli

Self-contained CLI for LLM-friendly web access: a single Rust binary (`web`) over
managed search and fetch APIs, driven by the `web` skill in
`~/dotfiles/pi/.config/pi/skills/web/`. No local browser, no MCP, no node
dependency in the binary.

## Install

```bash
cargo install --path .   # installs `web` to ~/.cargo/bin
```

## Subcommands

| Command | What it does | Providers (primary -> fallback) |
| ------- | ------------ | ------------------------------- |
| `web fetch <url>` | URL to clean markdown | Jina Reader -> Exa contents -> raw GET |
| `web search <query>` | Ranked list of links | Brave -> Exa (Firecrawl opt-in) |
| `web answer <query>` | Sourced answer + citations | Exa `/answer` -> Perplexity Sonar |
| `web crawl <url>` | Crawl a site to markdown | Firecrawl v2 |
| `web crawl <url> --map` | List discovered URLs | Firecrawl v2 |
| `web crawl <url> --depth N` | Depth-limited crawl | Firecrawl v2 |

`--json` on any command for structured output. See `web <cmd> --help`.

### Fetch chain

`web fetch` runs Jina Reader first, then Exa `/contents`, then a plain GET for
static pages that neither reader handles. A `jinaApiKey` raises Jina's limit from
20 requests a minute to 500 against a free allowance of 10 million tokens; without
one the command still works and says on stderr that it is on the keyless path. Exa
`/contents` bills about 1 USD per 1000 pages.

A raw GET that answers with a non-2xx status fails the command and prints the
status. An error page returned as if it were content is the failure mode this
chain exists to avoid.

`--raw` skips the readers and returns the response body verbatim.
`fetchBackbone` picks the head of the chain, `"jina"` (default) or `"exa"`.

### Search providers

`web search` runs on Brave and falls back to Exa when no Brave key is configured or
when Brave fails. Brave bills 5 USD per 1000 requests on its prepaid Search plan
and gives 5 USD of free credits a month, so roughly 1000 free searches; a card is
required since the standalone free tier was retired in February 2026.
`--provider brave|exa|firecrawl` pins one provider explicitly, which is also how
they get compared head to head.

Firecrawl is deliberately opt-in: it bills 2 credits per 10 results against a free
tier of roughly 1000 credits a month and 5 requests a minute, so it is a considered
choice rather than an automatic one. It answers with Firecrawl's search highlights,
query-relevant passages lifted from the page body that replace the usual one-line
description.

```bash
web search "firecrawl v2 search highlights" --provider firecrawl -n 5
```

Exa and Firecrawl return snippets that run to thousands of chars per result (Exa's
highlights reach ~25k chars over 5 results), so every result is capped at 500 chars
in the shared output path. `--max-snippet <chars>` raises or lowers that budget and
`--max-snippet 0` disables it for a verbose read.

```bash
web search "rust clap derive tutorial" -n 3 --max-snippet 0
```

### Crawl

Every crawl mode runs on Firecrawl v2 against a free tier of roughly 1000 credits a
month. Without `firecrawlApiKey` the command fails and names the missing key.
`--map` lists discovered URLs without fetching content, `--depth N` limits crawl
depth from the root, and `--limit N` caps the page count (default 20). A plain crawl
polls the async job until it completes, then concatenates the markdown.

## Config

`~/.config/web-cli/config.json`, camelCase keys, mode 600. The matching
upper-snake-case environment variables (`EXA_API_KEY`, `FIRECRAWL_API_KEY`,
`BRAVE_API_KEY`, `PERPLEXITY_API_KEY`, `JINA_API_KEY`) override the file values.

```json
{
  "exaApiKey": "...",
  "firecrawlApiKey": "...",
  "braveApiKey": "...",
  "perplexityApiKey": "...",
  "jinaApiKey": "...",
  "defaults": {
    "answerProvider": "exa",
    "perplexityModel": "sonar",
    "searchResults": 8,
    "saveThreshold": 30000,
    "fetchBackbone": "jina",
    "crawlBackbone": "firecrawl"
  }
}
```

`fetch` works with no key at all on Jina's keyless path. `search` needs `braveApiKey`
or `exaApiKey`, `crawl` needs `firecrawlApiKey`, and `answer` needs `exaApiKey` or
`perplexityApiKey`.

## Design notes

- Stateful logged-in browser sessions are out of scope for this tool by design; a
  browser hop in front of the providers cost a launch on every fetch and turned a
  failed launch into an indistinguishable "page not found".
- Output over `saveThreshold` chars is written to `$TMPDIR/web-cli/` and the path is
  printed, so an agent reads slices instead of flooding its context.
- Config path honours `$XDG_CONFIG_HOME` then `~/.config` (not the macOS Application
  Support dir the `directories` crate defaults to).
