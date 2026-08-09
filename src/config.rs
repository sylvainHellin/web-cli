use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// On-disk config at ~/.config/web-cli/config.json. Every field is optional;
/// env vars override file values at read time.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exa_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firecrawl_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perplexity_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jina_api_key: Option<String>,
    /// Override path to the crawl4ai `crwl` binary. If unset, resolution falls
    /// back to ~/.local/bin/crwl then $PATH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crawl4ai_bin: Option<String>,
    /// Override entry point for the chrome-devtools CLI used by `web browse`
    /// (a binary, or a .js script run through node). If unset, resolution falls
    /// back to `chrome-devtools` on $PATH, then `pnpm dlx` of browse_package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browse_bin: Option<String>,
    /// npm package spec for the `pnpm dlx` fallback of `web browse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browse_package: Option<String>,

    #[serde(default)]
    pub defaults: Defaults,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Defaults {
    /// Default provider for `web answer`: "exa" or "perplexity".
    #[serde(default = "default_answer_provider")]
    pub answer_provider: String,
    /// Perplexity Sonar model used when answer_provider = perplexity.
    #[serde(default = "default_perplexity_model")]
    pub perplexity_model: String,
    /// Number of results returned by `web search` by default.
    #[serde(default = "default_search_results")]
    pub search_results: usize,
    /// Char threshold above which fetch/crawl output is saved to a file
    /// instead of printed to stdout.
    #[serde(default = "default_save_threshold")]
    pub save_threshold: usize,
    /// Backbone for `web fetch`: "crawl4ai" (default, key-free) or "jina".
    #[serde(default = "default_fetch_backbone")]
    pub fetch_backbone: String,
    /// Backbone for `web crawl`: "crawl4ai" (default, key-free) or "firecrawl".
    #[serde(default = "default_crawl_backbone")]
    pub crawl_backbone: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            answer_provider: default_answer_provider(),
            perplexity_model: default_perplexity_model(),
            search_results: default_search_results(),
            save_threshold: default_save_threshold(),
            fetch_backbone: default_fetch_backbone(),
            crawl_backbone: default_crawl_backbone(),
        }
    }
}

fn default_answer_provider() -> String {
    "exa".to_string()
}
fn default_perplexity_model() -> String {
    "sonar".to_string()
}
fn default_search_results() -> usize {
    8
}
fn default_save_threshold() -> usize {
    30_000
}
fn default_fetch_backbone() -> String {
    "crawl4ai".to_string()
}
fn default_crawl_backbone() -> String {
    "crawl4ai".to_string()
}

impl Config {
    /// Pinned chrome-devtools-mcp spec for the `pnpm dlx` fallback.
    pub fn browse_package(&self) -> String {
        self.browse_package
            .clone()
            .unwrap_or_else(|| "chrome-devtools-mcp@1.6.0".to_string())
    }

    /// Path to the config file. Honours $XDG_CONFIG_HOME, else ~/.config
    /// (not the macOS "Application Support" dir, to match the rest of this
    /// user's tooling).
    pub fn path() -> Result<PathBuf> {
        let config_home = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => {
                let base = BaseDirs::new().context("Could not determine home directory")?;
                base.home_dir().join(".config")
            }
        };
        Ok(config_home.join("web-cli").join("config.json"))
    }

    /// Load config from disk (empty default if the file does not exist),
    /// then layer env-var overrides on top.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let mut cfg: Config = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config at {}", path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("Failed to parse config at {}", path.display()))?
        } else {
            Config::default()
        };

        // Env vars take precedence over file values.
        if let Ok(v) = std::env::var("EXA_API_KEY") {
            if !v.is_empty() {
                cfg.exa_api_key = Some(v);
            }
        }
        if let Ok(v) = std::env::var("FIRECRAWL_API_KEY") {
            if !v.is_empty() {
                cfg.firecrawl_api_key = Some(v);
            }
        }
        if let Ok(v) = std::env::var("PERPLEXITY_API_KEY") {
            if !v.is_empty() {
                cfg.perplexity_api_key = Some(v);
            }
        }
        if let Ok(v) = std::env::var("JINA_API_KEY") {
            if !v.is_empty() {
                cfg.jina_api_key = Some(v);
            }
        }

        Ok(cfg)
    }
}
