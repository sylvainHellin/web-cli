use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::config::Config;
use crate::crawl4ai;
use crate::http;
use crate::output;

/// Fetch a URL and return clean markdown.
///
/// Fallback chain:
///   0. crawl4ai (`crwl`) -- key-free local headless browser (default backbone)
///   1. Jina Reader (https://r.jina.ai/<url>) -- works key-free, adds key if present
///   2. Exa /contents -- if an Exa key is configured
///   3. Raw HTTP GET -- last resort, returns the body as-is
pub fn run(url: &str, raw: bool, json: bool) -> Result<()> {
    let cfg = Config::load()?;

    if raw {
        let body = raw_fetch(url)?;
        return emit(url, &body, "raw", json, cfg.defaults.save_threshold);
    }

    // 0. crawl4ai (default key-free backbone)
    if cfg.defaults.fetch_backbone == "crawl4ai" {
        if let Some(bin) = crawl4ai::resolve_bin(&cfg) {
            match crawl4ai::fetch(&bin, url) {
                Ok(md) => {
                    return emit(url, &md, "crawl4ai", json, cfg.defaults.save_threshold);
                }
                Err(e) => {
                    eprintln!("[web fetch] crawl4ai failed ({e}), trying Jina...")
                }
            }
        }
    }

    // 1. Jina Reader
    match jina_fetch(url, cfg.jina_api_key.as_deref()) {
        Ok(md) if !md.trim().is_empty() => {
            return emit(url, &md, "jina", json, cfg.defaults.save_threshold);
        }
        Ok(_) => eprintln!("[web fetch] Jina returned empty content, trying Exa..."),
        Err(e) => eprintln!("[web fetch] Jina failed ({e}), trying next provider..."),
    }

    // 2. Exa /contents
    if let Some(key) = cfg.exa_api_key.as_deref() {
        match exa_contents(url, key) {
            Ok(md) if !md.trim().is_empty() => {
                return emit(url, &md, "exa", json, cfg.defaults.save_threshold);
            }
            Ok(_) => eprintln!("[web fetch] Exa returned empty content, trying raw fetch..."),
            Err(e) => eprintln!("[web fetch] Exa failed ({e}), trying raw fetch..."),
        }
    }

    // 3. Raw fetch
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
fn jina_fetch(url: &str, api_key: Option<&str>) -> Result<String> {
    let client = http::client(60)?;
    let endpoint = format!("https://r.jina.ai/{url}");
    let mut req = client.get(&endpoint).header("X-Return-Format", "markdown");
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
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
fn raw_fetch(url: &str) -> Result<String> {
    let client = http::client(30)?;
    let resp = client.get(url).send().context("Raw fetch failed")?;
    let status = resp.status();
    let body = resp.text().context("Failed to read response body")?;
    if !status.is_success() {
        bail!("Raw fetch returned status {status}");
    }
    Ok(body)
}
