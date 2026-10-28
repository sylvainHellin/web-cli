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
