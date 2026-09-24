//! A small on-disk cache for fetched URL bodies used by the CLI.
//!
//! Fetching a page is the slow half of research, and agents often run several
//! extraction modes over the same URL in quick succession. This cache stores
//! each fetched [`FetchReport`] for a couple of minutes under the user's cache
//! directory so those follow-up parses reuse the bytes.
//!
//! The library's [`crate::fetch::fetch`] is never cached; only the CLI consults
//! this module. Cache reads and writes are best-effort: every failure is
//! swallowed because the cache is an optimization, never a correctness
//! requirement. Credential-bearing URLs, custom headers, non-`GET` requests,
//! request bodies, and `Cache-Control: no-store` all bypass it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fetch::FetchReport;

/// How long a cached fetch stays fresh.
pub const CACHE_TTL_SECS: u64 = 120;

#[derive(Serialize, Deserialize)]
struct CachedFetch {
    fetched_at: u64,
    report: FetchReport,
}

/// The cache directory: `$XDG_CACHE_HOME/darash/fetch` or `~/.cache/darash/fetch`.
///
/// Returns `None` when no home directory is available, which disables caching.
pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(base.join("darash").join("fetch"))
}

/// A stable, filesystem-safe cache key for a URL: truncated SHA-256 hex.
pub fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex.truncate(32);
    hex
}

/// Read a fresh cached report for `url`, returning it with its age in seconds.
pub fn load(url: &str) -> Option<(FetchReport, u64)> {
    let dir = cache_dir()?;
    let path = dir.join(format!("{}.json", cache_key(url)));
    let bytes = fs::read(&path).ok()?;
    let cached: CachedFetch = serde_json::from_slice(&bytes).ok()?;
    let now = now_secs();
    let age = now.saturating_sub(cached.fetched_at);
    if age > CACHE_TTL_SECS {
        let _ = fs::remove_file(&path);
        return None;
    }
    Some((cached.report, age))
}

/// Store a report for `url`, atomically and with `0600` permissions.
///
/// Returns `None` on any failure; callers ignore it.
pub async fn store(url: &str, report: &FetchReport) -> Option<()> {
    let dir = cache_dir()?;
    tokio::fs::create_dir_all(&dir).await.ok()?;

    let dir_clone = dir.clone();
    tokio::task::spawn_blocking(move || sweep(&dir_clone));

    let cached = CachedFetch {
        fetched_at: now_secs(),
        report: report.clone(),
    };
    let data = serde_json::to_vec(&cached).ok()?;
    let key = cache_key(url);
    let tmp = dir.join(format!(".{key}.tmp"));
    write_private(&tmp, &data).await.ok()?;
    tokio::fs::rename(&tmp, dir.join(format!("{key}.json"))).await.ok()?;
    Some(())
}

async fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut file = tokio::fs::File::create(path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600)).await?;
    }
    file.write_all(data).await?;
    file.sync_all().await?;
    Ok(())
}

/// Remove expired entries opportunistically; errors are ignored.
fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = now_secs();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.ends_with(".tmp") {
            let _ = fs::remove_file(&path);
            continue;
        }
        if !name.ends_with(".json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(cached) = serde_json::from_slice::<CachedFetch>(&bytes) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        if now.saturating_sub(cached.fetched_at) > CACHE_TTL_SECS {
            let _ = fs::remove_file(&path);
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Whether a request is eligible for the disk cache.
///
/// Custom headers, non-`GET` methods, request bodies, `Cache-Control: no-store`,
/// and credential-bearing URLs all bypass the cache.
pub fn should_cache(
    url: &str,
    method: &str,
    has_headers: bool,
    has_body: bool,
    no_store: bool,
) -> bool {
    method.eq_ignore_ascii_case("GET")
        && !has_headers
        && !has_body
        && !no_store
        && !is_credential_url(url)
}

/// Whether a URL's query parameters look credential-bearing.
///
/// Parameter names are snake-cased and matched as whole `_`-separated segments
/// against a small keyword set, so `access_token=…` and `apiKey=…` bypass the
/// cache while `monkey=…` does not.
pub fn is_credential_url(url: &str) -> bool {
    let Some((_, query)) = url.split_once('?') else {
        return false;
    };
    let query = query.split('#').next().unwrap_or(query);
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .any(|pair| is_credential_param(pair.split('=').next().unwrap_or("")))
}

/// Whether one query parameter name looks credential-bearing.
pub fn is_credential_param(name: &str) -> bool {
    let normalized = snake_case(name);
    normalized == "api_key"
        || normalized
            .split('_')
            .any(|segment| CREDENTIAL_SEGMENTS.contains(&segment))
}

const CREDENTIAL_SEGMENTS: [&str; 9] = [
    "token",
    "key",
    "secret",
    "signature",
    "sig",
    "credential",
    "password",
    "authorization",
    "auth",
];

fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (index, ch) in chars.iter().enumerate() {
        if ch.is_ascii_uppercase() {
            let previous = index.checked_sub(1).map(|i| chars[i]);
            let next = chars.get(index + 1);
            let after_lower_or_digit = previous
                .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                .unwrap_or(false);
            let acronym_end = previous.map(|c| c.is_ascii_uppercase()).unwrap_or(false)
                && next.map(|c| c.is_ascii_lowercase()).unwrap_or(false);
            if (after_lower_or_digit || acronym_end) && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if *ch == '-' || *ch == ' ' {
            out.push('_');
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_stable_and_url_specific() {
        let key = cache_key("https://example.com/a");
        assert_eq!(key.len(), 32);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, cache_key("https://example.com/a"));
        assert_ne!(key, cache_key("https://example.com/b"));
    }

    #[test]
    fn bypass_rules_cover_methods_headers_bodies_and_no_store() {
        assert!(should_cache(
            "https://example.com/a",
            "GET",
            false,
            false,
            false
        ));
        assert!(!should_cache(
            "https://example.com/a",
            "POST",
            false,
            false,
            false
        ));
        assert!(!should_cache(
            "https://example.com/a",
            "HEAD",
            false,
            false,
            false
        ));
        assert!(!should_cache(
            "https://example.com/a",
            "GET",
            true,
            false,
            false
        ));
        assert!(!should_cache(
            "https://example.com/a",
            "GET",
            false,
            true,
            false
        ));
        assert!(!should_cache(
            "https://example.com/a",
            "GET",
            false,
            false,
            true
        ));
    }

    #[test]
    fn credential_urls_bypass_and_whole_segments_only() {
        assert!(is_credential_url("https://example.com/?access_token=abc"));
        assert!(is_credential_url("https://example.com/?apiKey=abc"));
        assert!(is_credential_url("https://example.com/?a=1&secret=xyz"));
        assert!(is_credential_url(
            "https://example.com/?X-Amz-Signature=abc"
        ));
        assert!(is_credential_url("https://example.com/?password=hunter2"));
        assert!(!is_credential_url("https://example.com/?monkey=1"));
        assert!(!is_credential_url("https://example.com/?q=rust"));
        assert!(!is_credential_url("https://example.com/"));
        assert!(!should_cache(
            "https://example.com/?token=abc",
            "GET",
            false,
            false,
            false
        ));
    }

    #[test]
    fn credential_param_snake_cases_camel_case_and_acronyms() {
        assert!(is_credential_param("accessToken"));
        assert!(is_credential_param("auth_token"));
        assert!(is_credential_param("ACCESS_TOKEN"));
        assert!(is_credential_param("TOKEN"));
        assert!(is_credential_param("apiKey"));
        assert!(!is_credential_param("monkey"));
    }
}
