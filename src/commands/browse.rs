//! Stateful browser control: passthrough to the `chrome-devtools` CLI shipped
//! inside the chrome-devtools-mcp npm package. The CLI manages a persistent
//! daemon (`start` / `status` / `stop`) owning a Chrome instance; every other
//! command talks to it, so cookies and login state survive across calls.
//!
//! web-cli stays agnostic to the upstream command surface: args are forwarded
//! verbatim and upstream help is proxied. Only known noise banners are
//! stripped from the output.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

use crate::config::Config;

/// How to invoke the chrome-devtools CLI.
enum Runner {
    /// Direct entry point: a binary, or a .js script run through node.
    Bin(PathBuf),
    /// `pnpm dlx --package <pkg> chrome-devtools` fallback.
    PnpmDlx(String),
}

/// Resolution order: config `browseBin` -> `chrome-devtools` on $PATH ->
/// pinned `pnpm dlx` fallback (config `browsePackage`).
fn resolve_runner(cfg: &Config) -> Runner {
    if let Some(p) = cfg.browse_bin.as_deref() {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Runner::Bin(pb);
        }
    }
    if let Some(p) = which_in_path("chrome-devtools") {
        return Runner::Bin(p);
    }
    Runner::PnpmDlx(cfg.browse_package())
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

/// Known upstream noise: telemetry notices and the self-update banner.
/// Matched per line; everything else passes through untouched.
fn is_noise(line: &str) -> bool {
    const MARKERS: [&str; 10] = [
        "Google collects usage statistics",
        "To opt-out, run with --no-usage-statistics",
        "For more details, visit:",
        "Avoid sharing sensitive or personal information",
        "Performance tools may send trace URLs",
        "To disable, run with --no-performance-crux",
        "Update available:",
        "Run `npm install",
        "chrome-devtools-mcp exposes content of the browser",
        "debug, and modify any data in the browser",
    ];
    MARKERS.iter().any(|m| line.contains(m))
}

/// Drop noise lines and collapse the blank runs they leave behind.
fn filter_noise(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        if is_noise(line) {
            continue;
        }
        if line.trim().is_empty() && out.last().is_none_or(|l| l.trim().is_empty()) {
            continue;
        }
        out.push(line);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

pub fn run(args: &[String]) -> Result<()> {
    let cfg = Config::load()?;

    let mut cmd = match resolve_runner(&cfg) {
        Runner::Bin(p) if p.extension().is_some_and(|e| e == "js" || e == "mjs") => {
            let mut c = Command::new("node");
            c.arg(&p);
            c
        }
        Runner::Bin(p) => Command::new(p),
        Runner::PnpmDlx(pkg) => {
            if which_in_path("pnpm").is_none() {
                bail!(
                    "No chrome-devtools CLI found: set browseBin in the config, \
                     install chrome-devtools-mcp globally, or install pnpm for the dlx fallback"
                );
            }
            let mut c = Command::new("pnpm");
            c.args(["dlx", "--package", &pkg, "chrome-devtools"]);
            c
        }
    };

    let output = cmd
        .args(args)
        .output()
        .context("Failed to spawn the chrome-devtools CLI")?;

    let stdout = filter_noise(&String::from_utf8_lossy(&output.stdout));
    let stderr = filter_noise(&String::from_utf8_lossy(&output.stderr));
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }

    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_known_banners_and_collapses_blanks() {
        let input = "\nUpdate available: 1.1.1 -> 1.6.0\n\
                     Run `npm install chrome-devtools-mcp@latest` to update.\n\n\
                     Google collects usage statistics to improve Chrome DevTools MCP.\n\
                     For more details, visit: https://example.com\n\n\
                     # Page snapshot\nbutton \"Log in\" [uid=1_4]\n";
        let got = filter_noise(input);
        assert_eq!(got, "# Page snapshot\nbutton \"Log in\" [uid=1_4]\n");
    }

    #[test]
    fn keeps_real_output_untouched() {
        let input = "Started chrome-devtools-mcp\ndaemon pid: 123\n";
        assert_eq!(filter_noise(input), input);
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(filter_noise(""), "");
        assert_eq!(filter_noise("\n\n"), "");
    }
}
