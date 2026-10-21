# Decision log

## 2026-10-21 - Jina Reader runs keyless, and the CLI waits out its 20 requests a minute
Why: a paid key moves rate limiting and billing to the key, so every fetch would bill tokens and the free keyless allowance would go unused; 20 requests a minute covers agent sessions once a 429 waits and retries instead of failing.
Rejected: topping up the key ($50 for 1 billion tokens); keyless first with the key only after a 429.
Source: chat with Sylvain, 2026-10-21.
Status: accepted
