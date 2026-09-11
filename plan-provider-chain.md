# Plan: managed provider chain, no local browser

Scope of this plan: `web` becomes a search and fetch tool backed entirely by managed APIs.
Everything that drives a local browser leaves the repository.
Written 2026-09-11 against `442d8d3`, to be executed by another agent.

## Why

`web fetch` failed completely on 2026-09-11 and the failure traced to the local browser hop, not to any provider.

The `crwl` binary launches headless Chromium through its own Playwright, pinned to build `chromium-1223`, inside the uv tool venv at `~/.local/share/uv/tools/crawl4ai/`.
The Homebrew `playwright` on `$PATH` installs a different build (`chromium-1200`) into the same cache directory, so the obvious repair looks like it worked and changes nothing.
The correct repair was `~/.local/share/uv/tools/crawl4ai/bin/python -m playwright install chromium`, which is not discoverable from the error message.

Three further costs follow from having the browser first in the chain.
Every fetch pays a browser launch before falling through to providers that would have answered immediately.
A browser that fails to launch is indistinguishable, from the caller's side, from a page that does not exist.
And the raw GET at the end of the chain returned HTTP 404 for `https://w3id.org/dio`, a namespace that does exist, so a broken backbone produced a confident wrong answer instead of an error.

The full provider review, with pricing and quotas cited to primary sources, is at `/tmp/web-tooling-review.md`.
Copy it into the repository if it is still needed after this plan is executed; `/tmp` is wiped on reboot.

## Decisions

These are settled. Do not reopen them while executing.

Fetch runs on Jina Reader with a key, then Exa Contents, then a raw GET for static pages.
A key raises Jina from 20 requests per minute keyless to 500, and the free allowance is 10 million tokens.
Exa Contents bills about 1 USD per 1000 pages.

Search stays on Brave as the default, with Exa as fallback.
Brave's free tier covers roughly 1000 searches a month.

Crawl runs on Firecrawl v2, on the free tier of about 1000 credits a month.
`web crawl` stays as a subcommand.

Answer keeps Exa `/answer` with Perplexity Sonar behind it.
Unchanged by this plan.

Local browser support leaves `web` entirely.
Not behind a flag, not as a fallback, not as an opt-in backbone.
Stateful logged-in sessions are a real need and will get their own tool; this repository stops carrying them.

## Work list

Execute in order. Build after each step.

1. Delete the browser surface.
   Remove `src/commands/browse.rs`, `src/crawl4ai.rs`, and the `pw-browse/` directory.
   Remove the `Browse` variant from the `Commands` enum in `src/main.rs` and its match arm.
   Remove the `browse` module from `src/commands/mod.rs`.
   Check whether `rydoo-batch` depends on `browse` or on `pw-browse`; if it does, say so and stop rather than deleting a working automation.

2. Strip the crawl4ai configuration.
   In `src/config.rs`, remove `crawl4ai_bin`, `browse_bin`, `browse_package`, and the `browse_package()` method.
   Change `default_fetch_backbone()` to return `"jina"` and `default_crawl_backbone()` to return `"firecrawl"`.
   Keep both fields, since the backbone is still selectable.

3. Rewrite the fetch chain in `src/commands/fetch.rs`.
   Order is Jina Reader, Exa Contents, raw GET.
   Send `Authorization: Bearer <jinaApiKey>` when the key is present, and say on stderr that the keyless path is rate limited when it is not.
   A raw GET that returns a non-2xx status must fail the command rather than being reported as content, which is the silent-degradation bug above.

4. Rewrite the crawl path in `src/commands/crawl.rs` onto Firecrawl v2 for every mode.
   `--map` and `--depth` already run on Firecrawl; the plain crawl joins them and the `crwl` hop goes.
   Fail with a clear message naming `firecrawlApiKey` when no key is configured.

5. Confirm Brave is the search default in `src/commands/search.rs` and that Exa is the fallback, and correct the order if it is not.

6. Update `README.md`.
   The subcommand table still lists `web browse` and names crawl4ai as the primary for fetch and crawl.
   Rewrite the provider column, drop the `browse` row, and drop the `pw-browse` build instructions.
   Document the config file at `~/.config/web-cli/config.json`, its camelCase fields (`exaApiKey`, `firecrawlApiKey`, `braveApiKey`, `perplexityApiKey`, `jinaApiKey`), and that the matching upper-snake-case environment variables override the file.

7. Update the `web` skill at `~/dotfiles/pi/.config/pi/skills/web/SKILL.md`.
   It documents `web browse` and the crawl4ai backbone, and it is what an agent reads before using the tool, so a stale skill outlives a correct README.
   Load `authoring-agent-instructions` before editing it.

## Verification

`cargo build --release` and `cargo clippy` clean after each step.

Then, with keys configured, exercise each subcommand against a live target and confirm the provider that answered:

- `web fetch https://example.com`
- `web fetch https://w3id.org/dio`, which must not report 404; the namespace exists and this is the regression that motivated the plan
- `web search "iso 19650 naming convention" -n 3`
- `web crawl <a small docs site> --map`
- `web answer "what is w3id.org"`

`rg -n 'crawl4ai|crwl|pw-browse|browse' src/ README.md` must return nothing but unrelated prose.

## Out of scope

Do not add a new provider beyond the four named above.

Do not change `web answer`.

Do not replace the browser automation inside this repository.
That is a separate tool and a separate decision.

## Keys

Sylvain sets the keys himself; they are in `pass-cli`.
The config file `~/.config/web-cli/config.json` does not exist yet.
Do not write keys into the repository, into the README, or into any example.
