use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::http;

#[derive(Debug, Serialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    age: Option<String>,
}

/// Search the web for a ranked list of links.
///
/// Fallback chain: Brave (if keyed) -> Exa (if keyed).
pub fn run(
    query: &str,
    n: Option<usize>,
    site: Option<&str>,
    recency: Option<&str>,
    json: bool,
) -> Result<()> {
    let cfg = Config::load()?;
    let count = n.unwrap_or(cfg.defaults.search_results);

    // 1. Brave
    if let Some(key) = cfg.brave_api_key.as_deref() {
        match brave_search(query, count, site, recency, key) {
            Ok(results) if !results.is_empty() => {
                return emit(query, "brave", &results, json);
            }
            Ok(_) => eprintln!("[web search] Brave returned no results, trying Exa..."),
            Err(e) => eprintln!("[web search] Brave failed ({e}), trying Exa..."),
        }
    }

    // 2. Exa
    if let Some(key) = cfg.exa_api_key.as_deref() {
        let results = exa_search(query, count, site, recency, key)
            .context("Exa search failed")?;
        return emit(query, "exa", &results, json);
    }

    bail!("No search provider configured. Add braveApiKey or exaApiKey to the config.");
}

fn emit(query: &str, provider: &str, results: &[SearchResult], json: bool) -> Result<()> {
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

/// Brave Web Search: GET /res/v1/web/search.
fn brave_search(
    query: &str,
    count: usize,
    site: Option<&str>,
    recency: Option<&str>,
    api_key: &str,
) -> Result<Vec<SearchResult>> {
    let client = http::client(30)?;
    let q = match site {
        Some(s) => format!("site:{s} {query}"),
        None => query.to_string(),
    };
    let mut params: Vec<(String, String)> = vec![
        ("q".into(), q),
        ("count".into(), count.min(20).to_string()),
    ];
    if let Some(r) = recency {
        // Map our friendly names to Brave freshness codes.
        let fresh = match r {
            "day" => "pd",
            "week" => "pw",
            "month" => "pm",
            "year" => "py",
            other => other, // allow raw Brave codes / date ranges
        };
        params.push(("freshness".into(), fresh.into()));
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

    let mut results = Vec::new();
    if let Some(arr) = body["web"]["results"].as_array() {
        for r in arr {
            results.push(SearchResult {
                title: r["title"].as_str().unwrap_or("").to_string(),
                url: r["url"].as_str().unwrap_or("").to_string(),
                snippet: r["description"].as_str().unwrap_or("").to_string(),
                age: r["age"].as_str().map(|s| s.to_string()),
            });
        }
    }
    Ok(results)
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
