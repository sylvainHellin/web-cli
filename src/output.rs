use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Print markdown to stdout, or if it exceeds `threshold` chars, save it to a
/// temp `.md` file and print a short head plus the path so the agent can
/// `read` slices instead of flooding context. A threshold of 0 disables saving.
pub fn emit_or_save(content: &str, slug: &str, threshold: usize) -> Result<()> {
    if threshold == 0 || content.chars().count() <= threshold {
        println!("{content}");
        return Ok(());
    }

    let dir = std::env::temp_dir().join("web-cli");
    fs::create_dir_all(&dir).context("Failed to create temp dir")?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let safe: String = slug
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let safe = safe.trim_matches('-');
    let safe = if safe.is_empty() { "page" } else { safe };
    let path: PathBuf = dir.join(format!("{safe}-{ts}.md"));
    fs::write(&path, content).context("Failed to write output file")?;

    let head: String = content.chars().take(2_000).collect();
    let total = content.chars().count();
    println!(
        "[Content is {total} chars, saved to file. Read it with the `read` tool for full text or specific slices.]\n\
         Saved: {}\n\n--- first 2000 chars ---\n{head}",
        path.display()
    );
    Ok(())
}
