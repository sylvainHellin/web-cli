//! crawl4ai backbone: shells out to the bundled `crwl` CLI for key-free,
//! local fetch/crawl via a headless browser. No new crates; pure subprocess.
//!
//! Verified against crawl4ai 0.8.9:
//!   - `crwl <url> -o markdown` -> clean markdown on stdout
//!   - `crwl <url> -o all` -> full CrawlResult JSON (markdown at
//!     `markdown.raw_markdown`, title at `metadata.title`)
//!   - `crwl <url> -o all --deep-crawl bfs --max-pages N` -> JSON array of pages
//!
//! `-o json` is broken without an extraction strategy; do not use it.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

use crate::config::Config;

/// Resolve the `crwl` binary: config override -> ~/.local/bin/crwl -> $PATH.
/// Presence only; runtime failures are handled by the caller's fallback chain.
pub fn resolve_bin(cfg: &Config) -> Option<PathBuf> {
    if let Some(p) = cfg.crawl4ai_bin.as_deref() {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Some(base) = directories::BaseDirs::new() {
        let local = base.home_dir().join(".local").join("bin").join("crwl");
        if local.is_file() {
            return Some(local);
        }
    }
    which_in_path("crwl")
}

/// Minimal `$PATH` lookup (no extra crate).
fn which_in_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Run `crwl` with the given args, returning stdout. Non-zero exit -> Err with
/// captured stderr so the caller can log and fall through to a paid provider.
fn run_crwl(bin: &PathBuf, args: &[&str]) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("Failed to spawn {}", bin.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        bail!("crwl exited {code}: {}", stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Fetch a single URL as clean markdown via `crwl <url> -o markdown`.
pub fn fetch(bin: &PathBuf, url: &str) -> Result<String> {
    let md = run_crwl(bin, &[url, "-o", "markdown", "-bc"])?;
    if md.trim().is_empty() {
        bail!("crwl returned empty markdown");
    }
    Ok(md)
}

/// Deep-crawl a site, returning pages normalized to the
/// `{markdown, metadata:{title, sourceURL}}` shape the crawl formatter consumes.
///
/// Note: crawl4ai's CLI hardcodes max_depth=3; only page count is controllable
/// (via `--max-pages`).
pub fn crawl(bin: &PathBuf, url: &str, limit: usize) -> Result<Vec<serde_json::Value>> {
    let limit_s = limit.to_string();
    let raw = run_crwl(
        bin,
        &[
            url,
            "-o",
            "all",
            "--deep-crawl",
            "bfs",
            "--max-pages",
            &limit_s,
            "-bc",
        ],
    )?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).context("Failed to parse crwl JSON output")?;
    let pages = normalize_pages(parsed);
    if pages.is_empty() {
        bail!("crwl crawl returned no usable pages");
    }
    Ok(pages)
}

/// Normalize crwl `-o all` output (a single object or an array of page objects)
/// into `{markdown, metadata:{title, sourceURL}}` records, dropping pages with
/// empty markdown. Pure function: unit-testable without a subprocess.
fn normalize_pages(parsed: serde_json::Value) -> Vec<serde_json::Value> {
    // Deep crawl yields an array; a single fetch yields one object.
    let items: Vec<serde_json::Value> = match parsed {
        serde_json::Value::Array(a) => a,
        other => vec![other],
    };

    items
        .into_iter()
        .filter_map(|p| {
            let md = p
                .get("markdown")
                .and_then(|m| m.get("raw_markdown"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if md.trim().is_empty() {
                return None;
            }
            let title = p
                .get("metadata")
                .and_then(|m| m.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let src = p.get("url").and_then(|v| v.as_str()).unwrap_or("");
            Some(serde_json::json!({
                "markdown": md,
                "metadata": { "title": title, "sourceURL": src },
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_single_object() {
        let input = json!({
            "url": "https://example.com",
            "markdown": { "raw_markdown": "# Hi\nbody" },
            "metadata": { "title": "Example" },
        });
        let pages = normalize_pages(input);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0]["markdown"], "# Hi\nbody");
        assert_eq!(pages[0]["metadata"]["title"], "Example");
        assert_eq!(pages[0]["metadata"]["sourceURL"], "https://example.com");
    }

    #[test]
    fn normalizes_array_and_drops_empty_markdown() {
        let input = json!([
            { "url": "https://a.com", "markdown": { "raw_markdown": "a" }, "metadata": { "title": "A" } },
            { "url": "https://b.com", "markdown": { "raw_markdown": "   " }, "metadata": { "title": "B" } },
            { "url": "https://c.com", "markdown": { "raw_markdown": "c" } },
        ]);
        let pages = normalize_pages(input);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0]["metadata"]["sourceURL"], "https://a.com");
        // missing metadata.title defaults to empty string, not an error
        assert_eq!(pages[1]["metadata"]["title"], "");
        assert_eq!(pages[1]["metadata"]["sourceURL"], "https://c.com");
    }

    #[test]
    fn empty_when_no_usable_pages() {
        let input = json!([{ "url": "https://a.com", "markdown": { "raw_markdown": "" } }]);
        assert!(normalize_pages(input).is_empty());
    }
}
