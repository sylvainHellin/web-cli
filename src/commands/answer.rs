use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::json;

use crate::config::Config;
use crate::http;

#[derive(Debug, Serialize)]
struct Citation {
    title: String,
    url: String,
}

#[derive(Debug, Serialize)]
struct Answer {
    query: String,
    provider: String,
    answer: String,
    citations: Vec<Citation>,
}

/// Get a sourced answer with citations.
///
/// Provider defaults to config (`exa`), override with --provider.
/// Falls back exa <-> perplexity if the chosen one has no key.
pub fn run(query: &str, provider: Option<&str>, json: bool) -> Result<()> {
    let cfg = Config::load()?;
    let chosen = provider.unwrap_or(&cfg.defaults.answer_provider);

    let ans = match chosen {
        "perplexity" => {
            if let Some(key) = cfg.perplexity_api_key.as_deref() {
                perplexity_answer(query, &cfg.defaults.perplexity_model, key)?
            } else if let Some(key) = cfg.exa_api_key.as_deref() {
                eprintln!("[web answer] No Perplexity key, falling back to Exa...");
                exa_answer(query, key)?
            } else {
                bail!("No answer provider configured (need perplexityApiKey or exaApiKey).");
            }
        }
        _ => {
            // default: exa
            if let Some(key) = cfg.exa_api_key.as_deref() {
                exa_answer(query, key)?
            } else if let Some(key) = cfg.perplexity_api_key.as_deref() {
                eprintln!("[web answer] No Exa key, falling back to Perplexity...");
                perplexity_answer(query, &cfg.defaults.perplexity_model, key)?
            } else {
                bail!("No answer provider configured (need exaApiKey or perplexityApiKey).");
            }
        }
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&ans)?);
        return Ok(());
    }

    let mut out = format!("{}\n", ans.answer);
    if !ans.citations.is_empty() {
        out.push_str("\nSources:\n");
        for (i, c) in ans.citations.iter().enumerate() {
            if c.title.is_empty() {
                out.push_str(&format!("  [{}] {}\n", i + 1, c.url));
            } else {
                out.push_str(&format!("  [{}] {} -- {}\n", i + 1, c.title, c.url));
            }
        }
    }
    out.push_str(&format!("\n({})", ans.provider));
    println!("{out}");
    Ok(())
}

/// Exa /answer: returns an LLM answer plus citations.
fn exa_answer(query: &str, api_key: &str) -> Result<Answer> {
    let client = http::client(120)?;
    let payload = json!({ "query": query, "text": false });
    let resp = client
        .post("https://api.exa.ai/answer")
        .header("x-api-key", api_key)
        .json(&payload)
        .send()
        .context("Exa answer request failed")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().context("Failed to parse Exa response")?;
    if !status.is_success() {
        bail!("Exa returned status {status}: {body}");
    }

    let answer = body["answer"].as_str().unwrap_or("").to_string();
    let mut citations = Vec::new();
    if let Some(arr) = body["citations"].as_array() {
        for c in arr {
            citations.push(Citation {
                title: c["title"].as_str().unwrap_or("").to_string(),
                url: c["url"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    Ok(Answer {
        query: query.to_string(),
        provider: "exa".to_string(),
        answer,
        citations,
    })
}

/// Perplexity Sonar: chat completion with citations / search_results.
fn perplexity_answer(query: &str, model: &str, api_key: &str) -> Result<Answer> {
    let client = http::client(120)?;
    let payload = json!({
        "model": model,
        "messages": [{ "role": "user", "content": query }],
    });
    let resp = client
        .post("https://api.perplexity.ai/v1/sonar")
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .context("Perplexity request failed")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().context("Failed to parse Perplexity response")?;
    if !status.is_success() {
        bail!("Perplexity returned status {status}: {body}");
    }

    let answer = body["choices"]
        .get(0)
        .and_then(|c| c["message"]["content"].as_str())
        .unwrap_or("")
        .to_string();

    let mut citations = Vec::new();
    // Prefer the richer search_results; fall back to bare citations URLs.
    if let Some(arr) = body["search_results"].as_array() {
        for c in arr {
            citations.push(Citation {
                title: c["title"].as_str().unwrap_or("").to_string(),
                url: c["url"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    if citations.is_empty() {
        if let Some(arr) = body["citations"].as_array() {
            for c in arr {
                if let Some(u) = c.as_str() {
                    citations.push(Citation {
                        title: String::new(),
                        url: u.to_string(),
                    });
                }
            }
        }
    }

    Ok(Answer {
        query: query.to_string(),
        provider: "perplexity".to_string(),
        answer,
        citations,
    })
}
