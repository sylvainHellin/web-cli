use anyhow::{Context, Result};
use reqwest::blocking::Client;
use std::time::Duration;

const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/124.0 Safari/537.36 web-cli/0.1";

/// Build a blocking HTTP client with a sane default timeout and browser-like UA.
pub fn client(timeout_secs: u64) -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("Failed to build HTTP client")
}
