use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::config::Config;
use crate::content::{self, Body};
use crate::http;
use crate::output;

/// Fetch a URL and return clean markdown.
///
/// Chain: Jina Reader -> Exa /contents -> raw HTTP GET.
/// `fetchBackbone` picks the head of the chain ("jina" or "exa"); the rest of
/// the order is fixed. A raw GET that answers non-2xx fails the command: a 404
/// reported as page content is worse than no answer at all.
pub fn run(url: &str, raw: bool, json: bool) -> Result<()> {
    let cfg = Config::load()?;

    if raw {
        let body = raw_fetch(url)?;
        return emit(url, &body, "raw", json, cfg.defaults.save_threshold);
    }

    let order: [&str; 2] = match cfg.defaults.fetch_backbone.as_str() {
        "exa" => ["exa", "jina"],
        "jina" => ["jina", "exa"],
        other => {
            eprintln!("[web fetch] Unknown fetchBackbone '{other}', using jina.");
            ["jina", "exa"]
        }
    };

    for provider in order {
        let attempt = match provider {
            "jina" => {
                if cfg.jina_api_key.is_none() {
                    eprintln!(
                        "[web fetch] No jinaApiKey configured; the keyless Jina Reader path is \
                         rate limited to 20 requests/minute."
                    );
                }
                jina_fetch(url, cfg.jina_api_key.as_deref())
            }
            "exa" => match cfg.exa_api_key.as_deref() {
                Some(key) => exa_contents(url, key),
                None => {
                    eprintln!("[web fetch] No exaApiKey configured, skipping Exa.");
                    continue;
                }
            },
            _ => continue,
        };

        match attempt {
            Ok(md) if !md.trim().is_empty() => {
                return emit(url, &Body::Text(md), provider, json, cfg.defaults.save_threshold);
            }
            Ok(_) => eprintln!("[web fetch] {provider} returned empty content, falling through..."),
            Err(e) => eprintln!("[web fetch] {provider} failed ({e}), falling through..."),
        }
    }

    // Last resort: a raw GET, for static pages the readers choke on.
    let body = raw_fetch(url).context("All fetch providers failed")?;
    emit(url, &body, "raw", json, cfg.defaults.save_threshold)
}

fn emit(url: &str, body: &Body, provider: &str, json: bool, threshold: usize) -> Result<()> {
    let slug = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    match body {
        Body::Text(content) => {
            if json {
                let out = json!({
                    "url": url,
                    "provider": provider,
                    "chars": content.chars().count(),
                    "content": content,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
                Ok(())
            } else {
                output::emit_or_save(content, slug, threshold)
            }
        }
        Body::Binary {
            bytes,
            content_type,
            ext,
        } => {
            let path = output::save_bytes(bytes, slug, ext)?;
            if json {
                let out = json!({
                    "url": url,
                    "provider": provider,
                    "binary": true,
                    "path": path.display().to_string(),
                    "content_type": content_type,
                    "bytes": bytes.len(),
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!(
                    "[Binary content not printed; convert it with `doc parse <path>`.]\n\
                     Saved: {}\n\
                     Content-Type: {}\n\
                     Bytes: {}",
                    path.display(),
                    content_type.as_deref().unwrap_or("unknown"),
                    bytes.len()
                );
            }
            Ok(())
        }
    }
}

/// Jina Reader: GET https://r.jina.ai/<url>, returns markdown by default.
/// With a key the rate limit rises from 20 to 500 requests a minute. A key the
/// server refuses (401/402/403) gets one keyless retry, with a warning. A 429
/// on the keyless path waits and retries up to three times (3, 6, 12 s or the
/// server's Retry-After) before the chain moves on. A
/// target page Jina flags as an upstream error or CAPTCHA wall is an error, so
/// the chain moves on instead of returning the block page as content.
/// `X-No-Cache` makes Jina read the live page: its cache served a TYPO3 page's
/// old text a day after the edit.
fn jina_fetch(url: &str, api_key: Option<&str>) -> Result<String> {
    let client = http::client(60)?;
    let endpoint = format!("https://r.jina.ai/{url}");
    let send = |key: Option<&str>| -> Result<(reqwest::StatusCode, Option<String>, String)> {
        let mut req = client
            .get(&endpoint)
            .header("X-Return-Format", "markdown")
            .header("X-No-Cache", "true");
        if let Some(key) = key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let resp = req.send().context("Jina request failed")?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().context("Failed to read Jina response")?;
        Ok((status, retry_after, body))
    };

    let (mut status, mut retry_after, mut body) = send(api_key)?;
    let mut keyless = api_key.is_none();
    if !keyless && content::jina_key_rejected(status.as_u16()) {
        eprintln!(
            "[web fetch] jinaApiKey rejected ({status}); retried keyless (20 requests/minute)."
        );
        (status, retry_after, body) = send(None)?;
        keyless = true;
    }
    // The keyless limit is 20 requests a minute: wait out a burst here rather
    // than burn an Exa call or hand the rate limit back to the caller.
    let mut attempt = 1;
    while keyless && status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let Some(delay) = content::jina_retry_delay(attempt, retry_after.as_deref()) else {
            break;
        };
        eprintln!(
            "[web fetch] Jina keyless rate limit hit (20 requests/minute); waiting {} s, retry {attempt}.",
            delay.as_secs()
        );
        std::thread::sleep(delay);
        (status, retry_after, body) = send(None)?;
        attempt += 1;
    }
    if !status.is_success() {
        bail!("Jina returned status {status}");
    }
    if let Some(problem) = content::jina_upstream_problem(&body) {
        bail!("Jina reached a block or error page: {problem}");
    }
    Ok(body)
}

/// Exa /contents: POST with the URL, returns extracted text as markdown.
fn exa_contents(url: &str, api_key: &str) -> Result<String> {
    let client = http::client(60)?;
    let payload = json!({
        "urls": [url],
        "text": true,
    });
    let resp = client
        .post("https://api.exa.ai/contents")
        .header("x-api-key", api_key)
        .json(&payload)
        .send()
        .context("Exa request failed")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().context("Failed to parse Exa response")?;
    if !status.is_success() {
        bail!("Exa returned status {status}: {body}");
    }
    let text = body["results"]
        .get(0)
        .and_then(|r| r["text"].as_str())
        .unwrap_or("")
        .to_string();
    let title = body["results"]
        .get(0)
        .and_then(|r| r["title"].as_str())
        .unwrap_or("");
    if title.is_empty() {
        Ok(text)
    } else {
        Ok(format!("# {title}\n\n{text}"))
    }
}

/// Raw HTTP GET, returns the response body classified as text or binary.
/// Text is decoded per its charset; binary bytes are kept verbatim.
/// Non-2xx is an error, never content: an error page rendered as the answer is
/// the silent degradation this chain exists to avoid.
fn raw_fetch(url: &str) -> Result<Body> {
    // 120 s leaves room for a body near the cap on a modest connection.
    let client = http::client(120)?;
    let resp = client
        .get(url)
        .header("Accept", "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8")
        .send()
        .context("Raw fetch failed")?;
    let status = resp.status();
    let final_url = resp.url().to_string();
    if !status.is_success() {
        bail!("Raw fetch returned status {status} for {final_url}");
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if resp.content_length().is_some_and(|n| n > content::MAX_BODY_BYTES) {
        bail!(content::CAP_MESSAGE);
    }
    let bytes = content::read_capped(resp, content::MAX_BODY_BYTES)?;
    Ok(content::classify(content_type.as_deref(), bytes))
}
