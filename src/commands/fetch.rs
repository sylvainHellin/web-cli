use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::config::Config;
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
                return emit(url, &md, provider, json, cfg.defaults.save_threshold);
            }
            Ok(_) => eprintln!("[web fetch] {provider} returned empty content, falling through..."),
            Err(e) => eprintln!("[web fetch] {provider} failed ({e}), falling through..."),
        }
    }

    // Last resort: a raw GET, for static pages the readers choke on.
    let body = raw_fetch(url).context("All fetch providers failed")?;
    emit(url, &body, "raw", json, cfg.defaults.save_threshold)
}

fn emit(url: &str, content: &str, provider: &str, json: bool, threshold: usize) -> Result<()> {
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
        let slug = url
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        output::emit_or_save(content, slug, threshold)
    }
}

/// Jina Reader: GET https://r.jina.ai/<url>, returns markdown by default.
/// With a key the rate limit rises from 20 to 500 requests a minute.
fn jina_fetch(url: &str, api_key: Option<&str>) -> Result<String> {
    let client = http::client(60)?;
    let endpoint = format!("https://r.jina.ai/{url}");
    let mut req = client.get(&endpoint).header("X-Return-Format", "markdown");
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    let resp = req.send().context("Jina request failed")?;
    let status = resp.status();
    let body = resp.text().context("Failed to read Jina response")?;
    if !status.is_success() {
        bail!("Jina returned status {status}");
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

/// Raw HTTP GET, returns the response body verbatim.
/// Non-2xx is an error, never content: an error page rendered as the answer is
/// the silent degradation this chain exists to avoid.
fn raw_fetch(url: &str) -> Result<String> {
    let client = http::client(30)?;
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
    resp.text().context("Failed to read response body")
}
