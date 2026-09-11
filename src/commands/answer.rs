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
pub fn run(query: &str, provider: Option<&str>, lang: Option<&str>, json: bool) -> Result<()> {
    let cfg = Config::load()?;
    let chosen = provider.unwrap_or(&cfg.defaults.answer_provider);
    // CLI flag overrides the config default; unset means no language hint.
    let lang = lang.or(cfg.defaults.answer_lang.as_deref());

    let ans = match chosen {
        "perplexity" => {
            if let Some(key) = cfg.perplexity_api_key.as_deref() {
                perplexity_answer(query, &cfg.defaults.perplexity_model, lang, key)?
            } else if let Some(key) = cfg.exa_api_key.as_deref() {
                eprintln!("[web answer] No Perplexity key, falling back to Exa...");
                exa_answer(query, lang, key)?
            } else {
                bail!("No answer provider configured (need perplexityApiKey or exaApiKey).");
            }
        }
        _ => {
            // default: exa
            if let Some(key) = cfg.exa_api_key.as_deref() {
                exa_answer(query, lang, key)?
            } else if let Some(key) = cfg.perplexity_api_key.as_deref() {
                eprintln!("[web answer] No Exa key, falling back to Perplexity...");
                perplexity_answer(query, &cfg.defaults.perplexity_model, lang, key)?
            } else {
                bail!("No answer provider configured (need exaApiKey or perplexityApiKey).");
            }
        }
    };

    // Fail loudly on retrieval failure: a blank answer or no citations means the
    // provider returned nothing grounded, so do not print a confident-looking
    // empty success. Exit non-zero instead.
    if ans.answer.trim().is_empty() || ans.citations.is_empty() {
        bail!(
            "no grounded answer returned from {}; retrieval likely failed \
             (empty answer or no citations)",
            ans.provider
        );
    }

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
///
/// Note: Exa /answer takes no system prompt, so premise verification cannot be
/// injected here (unlike the Perplexity branch). The `lang` hint is plumbed
/// through but Exa /answer exposes no locale parameter, so it is currently a
/// no-op on this branch and kept only to keep the call signatures uniform.
fn exa_answer(query: &str, _lang: Option<&str>, api_key: &str) -> Result<Answer> {
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
fn perplexity_answer(query: &str, model: &str, lang: Option<&str>, api_key: &str) -> Result<Answer> {
    let client = http::client(120)?;
    // Premise verification: instruct the model to check assumptions baked into
    // the question, flag false premises, and admit insufficient sources rather
    // than guessing.
    let system = "Verify the premises embedded in the question before answering. \
        If a premise is false or unsupported, say so explicitly instead of \
        answering as if it were true. If the sources are insufficient to answer, \
        say so plainly rather than guessing.";
    let mut payload = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": query },
        ],
    });
    // Language hint via Sonar's web_search_options locale, when set.
    if let Some(code) = lang {
        payload["web_search_options"] = json!({ "user_location": { "locale": code } });
    }
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
