//! Silent self-update. At launch a background thread asks GitHub for the
//! latest release tag; if it is newer, it downloads the gzipped bare
//! executable (small, ~25MB, no need to re-download the 79MB model),
//! sanity-checks it and swaps it in place — Windows allows renaming a
//! running exe, so the next launch runs the new version.
//!
//! Everything fails silently: offline use is a core feature and the updater
//! must never nag or block. `autoupdate=0` in the config disables it.

use std::io::Read;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

const REPO: &str = "YSKM523/fastrans";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Update payloads larger than this are rejected (corrupt/wrong asset).
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

/// Removes the leftover previous binary from the last successful update.
pub fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
}

/// Fire-and-forget update check on a background thread.
pub fn spawn_check() {
    std::thread::spawn(|| {
        if let Err(e) = check_and_stage() {
            eprintln!("update check skipped: {e:#}");
        }
    });
}

fn check_and_stage() -> Result<()> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .build();
    let body = agent
        .get(&format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .set("User-Agent", "fastrans-updater")
        .call()?
        .into_string()?;
    let tag = extract_tag(&body).ok_or_else(|| anyhow!("no tag_name in release response"))?;
    let latest =
        parse_ver(tag.trim_start_matches('v')).ok_or_else(|| anyhow!("bad tag {tag:?}"))?;
    let current = parse_ver(CURRENT).expect("valid CARGO_PKG_VERSION");
    if latest <= current {
        return Ok(());
    }

    let url = format!("https://github.com/{REPO}/releases/download/{tag}/fastrans.exe.gz");
    let resp = agent
        .get(&url)
        .set("User-Agent", "fastrans-updater")
        .call()
        .with_context(|| format!("download {url}"))?;
    let mut gz = Vec::new();
    resp.into_reader()
        .take(MAX_DOWNLOAD)
        .read_to_end(&mut gz)?;
    let mut exe_bytes = Vec::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut exe_bytes)?;
    // Sanity: a real fastrans.exe is tens of MB and starts with the PE magic.
    if exe_bytes.len() < 10_000_000 || &exe_bytes[..2] != b"MZ" {
        return Err(anyhow!("downloaded payload does not look like fastrans.exe"));
    }

    let exe = std::env::current_exe()?;
    let staged = exe.with_extension("exe.new");
    std::fs::write(&staged, &exe_bytes)?;
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).context("rename running exe aside")?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        // Roll back so the install dir stays consistent.
        let _ = std::fs::rename(&old, &exe);
        return Err(e).context("move new exe into place");
    }
    eprintln!("self-updated to {tag}; takes effect on next launch");
    Ok(())
}

fn extract_tag(json: &str) -> Option<String> {
    let i = json.find("\"tag_name\"")?;
    let after = &json[i + "\"tag_name\"".len()..];
    let after = &after[after.find(':')? + 1..];
    let after = &after[after.find('"')? + 1..];
    Some(after[..after.find('"')?].to_string())
}

fn parse_ver(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split('.');
    let v = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    it.next().is_none().then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_json() {
        assert_eq!(
            extract_tag(r#"{"url":"x","tag_name":"v0.1.1","name":"y"}"#).as_deref(),
            Some("v0.1.1")
        );
        assert_eq!(
            extract_tag("{\n  \"tag_name\": \"v1.2.3\",\n}").as_deref(),
            Some("v1.2.3")
        );
        assert_eq!(extract_tag("{}"), None);
    }

    #[test]
    fn compares_versions() {
        assert_eq!(parse_ver("0.1.0"), Some((0, 1, 0)));
        assert!(parse_ver("0.10.2") > parse_ver("0.9.9"));
        assert_eq!(parse_ver("1.2"), None);
        assert_eq!(parse_ver("1.2.3.4"), None);
    }
}
