# Decision log

## 2026-10-21 - Jina Reader runs keyless, and the CLI waits out its 20 requests a minute
Why: a paid key moves rate limiting and billing to the key, so every fetch would bill tokens and the free keyless allowance would go unused; 20 requests a minute covers agent sessions once a 429 waits and retries instead of failing.
Rejected: topping up the key ($50 for 1 billion tokens); keyless first with the key only after a 429.
Source: chat with Sylvain, 2026-10-21.
Status: accepted

## 2026-10-28 - Every Jina fetch bypasses Jina's cache
Why: Jina's cache served the old text of an edited TYPO3 page a day after the change, while the raw GET and a browser showed the new one; an agent checking its own edit needs the live page.
Rejected: a `--fresh` flag, which leaves the stale default in place and relies on the caller knowing the page changed.
Source: chat with Sylvain, 2026-10-28.
Status: accepted

## 2026-11-06 - An Exa fetch appends the page's links from a raw GET
Why: Exa `/contents` returns plain text, so when Jina failed on the BMF Kaufpreisaufteilung page (503) the fetch lost the .xlsx download link and only the `--raw` HTML showed it; Exa's own `extras.links` returned one wrong link for that page.
Rejected: Exa `extras.links` (incomplete); a `--links` flag (leaves the default silently lossy); a local HTML-to-markdown crate for the raw path (a new dependency for a narrower gap).
Source: chat with Sylvain, 2026-11-06.
Status: accepted
