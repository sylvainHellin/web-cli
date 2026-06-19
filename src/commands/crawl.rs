use anyhow::{bail, Context, Result};
use serde_json::json;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::crawl4ai;
use crate::http;
use crate::output;

/// Crawl a site.
///   --map  : fast, returns the list of discovered URLs (Firecrawl /map, paid)
///   --depth: depth-limited crawl (Firecrawl only, paid)
///   default: crawl pages to markdown via crawl4ai (key-free) -> Firecrawl fallback
pub fn run(url: &str, map: bool, limit: usize, depth: Option<usize>, json: bool) -> Result<()> {
    let cfg = Config::load()?;

    // crawl4ai backbone: key-free page crawl. Not used for --map (no cheap
    // URL-discovery mode) or --depth (crwl hardcodes max_depth=3).
    let depth_forces_firecrawl = depth.is_some();
    if !map
        && cfg.defaults.crawl_backbone == "crawl4ai"
        && !depth_forces_firecrawl
    {
        if let Some(bin) = crawl4ai::resolve_bin(&cfg) {
            match crawl4ai::crawl(&bin, url, limit) {
                Ok(pages) => {
                    return emit_pages(url, &pages, json, cfg.defaults.save_threshold);
                }
                Err(e) => {
                    eprintln!("[web crawl] crawl4ai failed ({e}), trying Firecrawl...")
                }
            }
        }
    } else if !map && depth_forces_firecrawl && cfg.defaults.crawl_backbone == "crawl4ai" {
        eprintln!("[web crawl] --depth not configurable via crawl4ai; using Firecrawl (needs firecrawlApiKey).");
    }

    let key = cfg
        .firecrawl_api_key
        .as_deref()
        .context("Firecrawl not configured. Add firecrawlApiKey to the config.")?;

    if map {
        map_urls(url, limit, key, json)
    } else {
        crawl_pages(url, limit, depth, key, json, cfg.defaults.save_threshold)
    }
}

/// Render a list of normalized `{markdown, metadata:{title, sourceURL}}` pages,
/// shared by the crawl4ai and Firecrawl paths.
fn emit_pages(
    url: &str,
    pages: &[serde_json::Value],
    json: bool,
    threshold: usize,
) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "url": url,
                "page_count": pages.len(),
                "pages": pages,
            }))?
        );
        return Ok(());
    }

    let mut out = format!("Crawl: {url}  ({} pages)\n", pages.len());
    for p in pages {
        let md = p["markdown"].as_str().unwrap_or("");
        let src = p["metadata"]["sourceURL"]
            .as_str()
            .or_else(|| p["metadata"]["url"].as_str())
            .unwrap_or("");
        let title = p["metadata"]["title"].as_str().unwrap_or("");
        out.push_str(&format!("\n\n{}\n# {title}\n<{src}>\n\n{md}", "=".repeat(60)));
    }

    let slug = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    output::emit_or_save(&out, &format!("crawl-{slug}"), threshold)
}

/// Firecrawl /map: discover URLs on a site. Synchronous.
fn map_urls(url: &str, limit: usize, api_key: &str, json: bool) -> Result<()> {
    let client = http::client(120)?;
    let payload = json!({ "url": url, "limit": limit });
    let resp = client
        .post("https://api.firecrawl.dev/v2/map")
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .context("Firecrawl map request failed")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().context("Failed to parse Firecrawl response")?;
    if !status.is_success() {
        bail!("Firecrawl /map returned status {status}: {body}");
    }

    // Response: { success, links: [ { url, title?, description? } | "url" ] }
    let links = body["links"].as_array().cloned().unwrap_or_default();

    if json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "url": url,
            "count": links.len(),
            "links": links,
        }))?);
        return Ok(());
    }

    let mut out = format!("Map: {url}  ({} URLs)\n", links.len());
    for l in &links {
        let u = l["url"].as_str().or_else(|| l.as_str()).unwrap_or("");
        if u.is_empty() {
            continue;
        }
        out.push_str(&format!("  {u}\n"));
    }
    println!("{out}");
    Ok(())
}

/// Firecrawl /crawl: async crawl, poll until complete, concatenate markdown.
fn crawl_pages(
    url: &str,
    limit: usize,
    depth: Option<usize>,
    api_key: &str,
    json: bool,
    threshold: usize,
) -> Result<()> {
    let client = http::client(120)?;

    let mut payload = json!({
        "url": url,
        "limit": limit,
        "scrapeOptions": { "formats": ["markdown"], "onlyMainContent": true },
    });
    if let Some(d) = depth {
        payload["maxDiscoveryDepth"] = json!(d);
    }

    // Kick off the crawl job.
    let resp = client
        .post("https://api.firecrawl.dev/v2/crawl")
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .context("Firecrawl crawl request failed")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().context("Failed to parse Firecrawl response")?;
    if !status.is_success() {
        bail!("Firecrawl /crawl returned status {status}: {body}");
    }
    let job_id = body["id"]
        .as_str()
        .context("Firecrawl did not return a crawl job id")?
        .to_string();

    eprintln!("[web crawl] Job {job_id} started, polling...");

    // Poll GET /crawl/{id} until status == completed (cap at ~5 min).
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut pages: Vec<serde_json::Value> = Vec::new();
    loop {
        if Instant::now() > deadline {
            bail!("Crawl timed out after 5 minutes (job {job_id})");
        }
        sleep(Duration::from_secs(3));

        let poll = client
            .get(format!("https://api.firecrawl.dev/v2/crawl/{job_id}"))
            .bearer_auth(api_key)
            .send()
            .context("Firecrawl poll request failed")?;
        let poll_status = poll.status();
        let pbody: serde_json::Value =
            poll.json().context("Failed to parse Firecrawl poll response")?;
        if !poll_status.is_success() {
            bail!("Firecrawl poll returned status {poll_status}: {pbody}");
        }

        let state = pbody["status"].as_str().unwrap_or("");
        match state {
            "completed" => {
                if let Some(arr) = pbody["data"].as_array() {
                    pages = arr.clone();
                }
                break;
            }
            "failed" | "cancelled" => {
                bail!("Crawl {state} (job {job_id}): {pbody}");
            }
            _ => {
                let done = pbody["completed"].as_u64().unwrap_or(0);
                let total = pbody["total"].as_u64().unwrap_or(0);
                eprintln!("[web crawl] {state}: {done}/{total} pages...");
            }
        }
    }

    emit_pages(url, &pages, json, threshold)
}
