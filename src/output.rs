use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Print markdown to stdout, or if it exceeds `threshold` chars, save it to a
/// temp `.md` file and print a short head plus the path so the agent can
/// `read` slices instead of flooding context. A threshold of 0 disables saving.
pub fn emit_or_save(content: &str, slug: &str, threshold: usize) -> Result<()> {
    if threshold == 0 || content.chars().count() <= threshold {
        println!("{content}");
        return Ok(());
    }

    let path = spill_path(slug, "md")?;
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

/// Write raw bytes verbatim to a temp file named after `slug` with extension
/// `ext`, and return its path.
pub fn save_bytes(bytes: &[u8], slug: &str, ext: &str) -> Result<PathBuf> {
    let path = spill_path(slug, ext)?;
    fs::write(&path, bytes).context("Failed to write output file")?;
    Ok(path)
}

/// Build `$TMPDIR/web-cli/<sanitised-slug>-<unix-ts>.<ext>`, creating the dir.
fn spill_path(slug: &str, ext: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("web-cli");
    fs::create_dir_all(&dir).context("Failed to create temp dir")?;
    Ok(spill_name(&dir, slug, ext))
}

/// Longest sanitised slug kept in a spill filename, in bytes, so long URLs
/// (pre-signed links, big queries) stay under the 255-byte filename limit.
const MAX_SLUG_BYTES: usize = 100;

fn spill_name(dir: &Path, slug: &str, ext: &str) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let safe: String = slug
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let safe = safe.trim_matches('-');
    // `is_alphanumeric` keeps non-ASCII letters, so cut on a char boundary.
    let mut end = safe.len().min(MAX_SLUG_BYTES);
    while !safe.is_char_boundary(end) {
        end -= 1;
    }
    let safe = safe[..end].trim_end_matches('-');
    let safe = if safe.is_empty() { "page" } else { safe };
    dir.join(format!("{safe}-{ts}.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_name(p: &Path) -> String {
        p.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn spill_name_sanitises_slug_and_applies_ext() {
        let dir = Path::new("/tmp/x");
        let name = file_name(&spill_name(dir, "example.com/a b?c=1", "pdf"));
        assert!(name.starts_with("example-com-a-b-c-1-"), "{name}");
        assert!(name.ends_with(".pdf"), "{name}");
    }

    #[test]
    fn spill_name_bounds_long_slugs() {
        let dir = Path::new("/tmp/x");
        let prefix = "https://bucket.s3.amazonaws.com/f.pdf?";
        let url = format!("{prefix}{}", "X".repeat(400 - prefix.len()));
        let name = file_name(&spill_name(dir, &url, "pdf"));
        assert!(name.len() < 255, "{} bytes: {name}", name.len());
        assert!(
            name.starts_with("https---bucket-s3-amazonaws-com-f-pdf-XXX"),
            "{name}"
        );
        assert!(name.ends_with(".pdf"), "{name}");

        let wide = "é".repeat(400);
        let name = file_name(&spill_name(dir, &wide, "md"));
        assert!(name.len() < 255, "{} bytes", name.len());
        assert!(name.ends_with(".md"), "{name}");
    }

    #[test]
    fn spill_name_empty_slug_falls_back_to_page() {
        let name = file_name(&spill_name(Path::new("/tmp/x"), "///", "md"));
        assert!(name.starts_with("page-"), "{name}");
        assert!(name.ends_with(".md"), "{name}");
    }

    #[test]
    fn spill_path_lives_in_web_cli_temp_dir() {
        let p = spill_path("a", "bin").unwrap();
        assert_eq!(p.parent().unwrap(), std::env::temp_dir().join("web-cli"));
    }
}
