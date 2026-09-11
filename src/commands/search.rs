use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::http;

#[derive(Debug, Clone, Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    age: Option<String>,
}

/// Search the web for a ranked list of links.
///
/// Default provider: Brave, with Exa as the fallback when no Brave key is
/// configured or Brave fails. Firecrawl is opt-in only (`--provider firecrawl`):
/// it bills 2 credits per 10 results, so it stays out of the default path.
pub fn run(
    query: &str,
    n: Option<usize>,
    site: Option<&str>,
    recency: Option<&str>,
    provider: Option<&str>,
    max_snippet: usize,
    json: bool,
) -> Result<()> {
    let cfg = Config::load()?;
    let count = n.unwrap_or(cfg.defaults.search_results);

    // Explicit provider: use it and only it, so a comparison run is honest.
    match provider {
        Some("firecrawl") => {
            let key = cfg
                .firecrawl_api_key
                .as_deref()
                .context("Firecrawl not configured. Add firecrawlApiKey to the config.")?;
            let results = firecrawl_search(query, count, site, recency, key)
                .context("Firecrawl search failed")?;
            return emit(query, "firecrawl", &results, max_snippet, json);
        }
        Some("exa") => {
            let key = cfg
                .exa_api_key
                .as_deref()
                .context("Exa not configured. Add exaApiKey to the config.")?;
            let results = exa_search(query, count, site, recency, key)
                .context("Exa search failed")?;
            return emit(query, "exa", &results, max_snippet, json);
        }
        Some("brave") => {
            let key = cfg
                .brave_api_key
                .as_deref()
                .context("Brave not configured. Add braveApiKey to the config.")?;
            let results = brave_search(query, count, site, recency, key)
                .context("Brave search failed")?;
            return emit(query, "brave", &results, max_snippet, json);
        }
        Some(other) => {
            bail!("Unknown search provider '{other}' (expected brave, exa or firecrawl)")
        }
        None => {}
    }

    // Default: Brave, then Exa.
    let mut brave_error: Option<String> = None;
    if let Some(key) = cfg.brave_api_key.as_deref() {
        match brave_search(query, count, site, recency, key) {
            Ok(results) if !results.is_empty() => {
                return emit(query, "brave", &results, max_snippet, json);
            }
            Ok(_) => eprintln!("[web search] Brave returned no results, trying Exa..."),
            Err(e) => {
                eprintln!("[web search] Brave failed ({e}), trying Exa...");
                brave_error = Some(e.to_string());
            }
        }
    }

    if let Some(key) = cfg.exa_api_key.as_deref() {
        let results = exa_search(query, count, site, recency, key)
            .context("Exa search failed")?;
        return emit(query, "exa", &results, max_snippet, json);
    }

    match brave_error {
        Some(e) => bail!("Brave search failed ({e}) and no exaApiKey is configured as fallback."),
        None => bail!("No search provider configured. Add braveApiKey (or exaApiKey) to the config."),
    }
}

/// Brave /web/search: GET, the default provider. Billed at 5 USD per 1000
/// requests on the prepaid Search plan, 50 requests per second, with 5 USD of
/// free credits a month. A card is required; the standalone free tier was
/// retired in February 2026.
fn brave_search(
    query: &str,
    count: usize,
    site: Option<&str>,
    recency: Option<&str>,
    api_key: &str,
) -> Result<Vec<SearchResult>> {
    let client = http::client(30)?;
    // Brave has no domain parameter, so the `site:` operator scopes the query.
    let q = match site {
        Some(s) => format!("site:{s} {query}"),
        None => query.to_string(),
    };
    // The API caps `count` at 20 per request.
    let count = count.clamp(1, 20);
    let mut params: Vec<(&str, String)> = vec![("q", q), ("count", count.to_string())];
    if let Some(r) = recency {
        let freshness = match r {
            "day" => "pd",
            "week" => "pw",
            "month" => "pm",
            "year" => "py",
            other => other, // allow raw freshness codes and date ranges
        };
        params.push(("freshness", freshness.to_string()));
    }

    let resp = client
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("Accept", "application/json")
        .header("X-Subscription-Token", api_key)
        .query(&params)
        .send()
        .context("Brave request failed")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().context("Failed to parse Brave response")?;
    if !status.is_success() {
        bail!("Brave returned status {status}: {body}");
    }

    // Response: { web: { results: [ { title, url, description, age, page_age } ] } }
    let arr = body["web"]["results"].as_array().cloned().unwrap_or_default();
    let mut results = Vec::new();
    for r in &arr {
        let age = r["age"]
            .as_str()
            .or_else(|| r["page_age"].as_str())
            .map(|s| s.to_string());
        results.push(SearchResult {
            title: strip_html(r["title"].as_str().unwrap_or("")),
            url: r["url"].as_str().unwrap_or("").to_string(),
            // Brave marks query terms with <strong> in the description.
            snippet: strip_html(r["description"].as_str().unwrap_or("")),
            age,
        });
    }
    Ok(results)
}

/// Drop HTML tags and decode the handful of entities Brave emits, so snippets
/// read as plain text.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// Render results, applying the shared snippet cap first so every provider
/// pays the same context budget. `max_snippet == 0` means uncapped.
fn emit(
    query: &str,
    provider: &str,
    results: &[SearchResult],
    max_snippet: usize,
    json: bool,
) -> Result<()> {
    let mut results = results.to_vec();
    if max_snippet > 0 {
        for r in &mut results {
            r.snippet = truncate_chars(&r.snippet, max_snippet);
        }
    }

    if json {
        let out = json!({
            "query": query,
            "provider": provider,
            "result_count": results.len(),
            "results": results,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    let mut out = format!("Search: \"{query}\"  ({provider}, {} results)\n", results.len());
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("\n{}. {}\n   {}\n", i + 1, r.title, r.url));
        if let Some(age) = &r.age {
            out.push_str(&format!("   [{age}]\n"));
        }
        if !r.snippet.is_empty() {
            out.push_str(&format!("   {}\n", r.snippet));
        }
    }
    out.push_str("\nOpen any result with: web fetch <url>");
    println!("{out}");
    Ok(())
}

/// Exa /search: POST with highlights for snippet text.
fn exa_search(
    query: &str,
    count: usize,
    site: Option<&str>,
    recency: Option<&str>,
    api_key: &str,
) -> Result<Vec<SearchResult>> {
    let client = http::client(30)?;
    let mut payload = json!({
        "query": query,
        "type": "auto",
        "numResults": count,
        "contents": { "highlights": true },
    });
    if let Some(s) = site {
        payload["includeDomains"] = json!([s]);
    }
    if let Some(r) = recency {
        // Translate recency to a startPublishedDate (approximate, UTC days back).
        if let Some(days) = match r {
            "day" => Some(1i64),
            "week" => Some(7),
            "month" => Some(30),
            "year" => Some(365),
            _ => None,
        } {
            if let Some(date) = days_ago_iso(days) {
                payload["startPublishedDate"] = json!(date);
            }
        }
    }

    let resp = client
        .post("https://api.exa.ai/search")
        .header("x-api-key", api_key)
        .json(&payload)
        .send()
        .context("Exa request failed")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().context("Failed to parse Exa response")?;
    if !status.is_success() {
        bail!("Exa returned status {status}: {body}");
    }

    let mut results = Vec::new();
    if let Some(arr) = body["results"].as_array() {
        for r in arr {
            let snippet = r["highlights"]
                .as_array()
                .and_then(|h| h.first())
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            results.push(SearchResult {
                title: r["title"].as_str().unwrap_or("").to_string(),
                url: r["url"].as_str().unwrap_or("").to_string(),
                snippet,
                age: r["publishedDate"]
                    .as_str()
                    .map(|s| s.chars().take(10).collect()),
            });
        }
    }
    Ok(results)
}

/// Firecrawl v2 /search: POST with highlights (on by default) for snippet text.
/// Opt-in provider, billed at 2 credits per 10 results.
fn firecrawl_search(
    query: &str,
    count: usize,
    site: Option<&str>,
    recency: Option<&str>,
    api_key: &str,
) -> Result<Vec<SearchResult>> {
    let client = http::client(60)?;
    // Firecrawl passes the query through to the underlying engine, so the
    // `site:` operator is the cheapest way to scope it.
    let q = match site {
        Some(s) => format!("site:{s} {query}"),
        None => query.to_string(),
    };
    let mut payload = json!({
        "query": q,
        "limit": count,
        "sources": ["web"],
        // On by default server-side, but pinned here so the snippet source is
        // explicit: highlights replace `description` with query-relevant
        // passages lifted from the page (same 2-credits-per-10-results cost).
        "highlights": true,
    });
    if let Some(r) = recency {
        // Google-style time-based search codes (tbs).
        let tbs = match r {
            "day" => "qdr:d",
            "week" => "qdr:w",
            "month" => "qdr:m",
            "year" => "qdr:y",
            other => other, // allow raw tbs codes
        };
        payload["tbs"] = json!(tbs);
    }

    let resp = client
        .post("https://api.firecrawl.dev/v2/search")
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .context("Firecrawl request failed")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().context("Failed to parse Firecrawl response")?;
    if !status.is_success() {
        bail!("Firecrawl /search returned status {status}: {body}");
    }

    // Response: { success, data: { web: [ { url, title, description, ... } ] } }
    let arr = body["data"]["web"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let mut results = Vec::new();
    for r in &arr {
        // With highlights enabled the API may return query-relevant snippets
        // in place of (or alongside) the plain description.
        // Firecrawl returns highlights in place of `description` rather than as
        // a separate field; the array form is handled anyway in case that
        // changes. Length is handled centrally in `emit`.
        let snippet = r["highlights"]
            .as_array()
            .and_then(|h| h.first())
            .and_then(|s| s.as_str())
            .or_else(|| r["description"].as_str())
            .unwrap_or("");
        results.push(SearchResult {
            title: r["title"].as_str().unwrap_or("").to_string(),
            url: r["url"].as_str().unwrap_or("").to_string(),
            snippet: snippet.to_string(),
            age: None,
        });
    }
    Ok(results)
}

/// Truncate on a char boundary, appending an ellipsis when text was cut.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("...");
    out
}

/// Compute an ISO 8601 (UTC, date-only) string `days` before now, without
/// pulling in a date crate. Returns None on arithmetic failure.
fn days_ago_iso(days: i64) -> Option<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let then = now - days * 86_400;
    if then < 0 {
        return None;
    }
    Some(civil_date_from_unix(then))
}

/// Convert a unix timestamp (seconds) to a `YYYY-MM-DD` string (UTC).
/// Uses the standard days-from-civil algorithm (Howard Hinnant).
fn civil_date_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
