//! Background update checker.
//!
//! After each CLI invocation we spawn a non-blocking check against the
//! `version.json` published as a GitHub Release asset.  If a newer version exists we print a
//! one-line notice to stderr.  The check is skipped if the `PZ_TUNNEL_NO_UPDATE_CHECK`
//! env var is set, or if the last check was less than 24 hours ago.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde::Deserialize;
use tokio::time::timeout;

const RELEASES_BASE_URL: &str = "https://github.com/PortZeroNetwork/portzero/releases";

/// Minimum interval between remote checks.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Timeout for the HTTP request so it never blocks the CLI noticeably.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

/// Shape of the version manifest uploaded alongside release artifacts.
#[derive(Deserialize)]
struct VersionManifest {
    version: String,
}

/// Path to the timestamp file that records when we last checked.
fn last_check_path() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    Ok(home.join(".portzero").join(".last_update_check"))
}

/// Return true if enough time has elapsed since the last check.
fn should_check() -> bool {
    if std::env::var("PZ_TUNNEL_NO_UPDATE_CHECK").is_ok() {
        return false;
    }

    let path = match last_check_path() {
        Ok(p) => p,
        Err(_) => return true,
    };

    let metadata = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(_) => return true,
    };

    let modified = match metadata.modified() {
        Ok(t) => t,
        Err(_) => return true,
    };

    SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO)
        >= CHECK_INTERVAL
}

/// Touch the timestamp file so we don't check again for 24 h.
fn record_check() {
    if let Ok(path) = last_check_path() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, []).ok();
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    prerelease: Option<String>,
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)) {
            Ordering::Equal => compare_prerelease(&self.prerelease, &other.prerelease),
            ordering => ordering,
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prerelease(left: &Option<String>, right: &Option<String>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => compare_prerelease_identifiers(left, right),
    }
}

fn compare_prerelease_identifiers(left: &str, right: &str) -> Ordering {
    let mut left_parts = left.split('.');
    let mut right_parts = right.split('.');

    loop {
        match (left_parts.next(), right_parts.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => {
                let left_num = left.parse::<u64>();
                let right_num = right.parse::<u64>();
                let ordering = match (left_num, right_num) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => left.cmp(right),
                };
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
        }
    }
}

/// Parse a SemVer string, ignoring any leading `v`.
fn parse_semver(s: &str) -> Option<Version> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let (core, prerelease) = match s.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease.to_string())),
        None => (s, None),
    };
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Version {
        major,
        minor,
        patch,
        prerelease,
    })
}

fn version_url() -> String {
    // `version.json` is uploaded as a GitHub Release asset by
    // .github/workflows/release.yml; GitHub's /releases/latest/download/<asset>
    // always resolves to the newest stable release. GitHub exposes no static
    // "latest prerelease" URL and this repo publishes no separate staging
    // channel, so all builds track the latest stable release.
    format!("{RELEASES_BASE_URL}/latest/download/version.json")
}

/// Run the update check.  Intended to be called with `tokio::spawn` so it
/// never delays the main command.
pub async fn check_for_update() {
    if !should_check() {
        return;
    }

    let current = env!("CARGO_PKG_VERSION");
    let Some(current_version) = parse_semver(current) else {
        return;
    };

    if let Ok(Ok(Some(latest))) = timeout(REQUEST_TIMEOUT, fetch_latest_version()).await {
        record_check();

        if let Some(latest_version) = parse_semver(&latest) {
            if latest_version > current_version {
                eprintln!(
                    "\x1b[33mA new version of portzero is available: {current} -> {latest}\x1b[0m"
                );
                eprintln!("\x1b[33mUpdate with: portzero update\x1b[0m");
            }
        }
    }
}

async fn fetch_latest_version() -> Result<Option<String>> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    let resp = client.get(version_url()).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let manifest: VersionManifest = resp.json().await?;
    Ok(Some(manifest.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_releases_base_url_points_to_correct_repo() {
        // Verify the URL points to the correct organization and repo, not the old one
        assert!(
            RELEASES_BASE_URL.contains("PortZeroNetwork/portzero"),
            "RELEASES_BASE_URL must contain PortZeroNetwork/portzero, got: {}",
            RELEASES_BASE_URL
        );
        assert!(
            !RELEASES_BASE_URL.contains("LoumTechnologies"),
            "RELEASES_BASE_URL must not contain LoumTechnologies (old org)"
        );
        assert!(
            !RELEASES_BASE_URL.contains("port-zero"),
            "RELEASES_BASE_URL must not contain 'port-zero' (old repo name)"
        );
    }

    #[test]
    fn test_version_url_format() {
        let url = version_url();
        assert_eq!(
            url,
            "https://github.com/PortZeroNetwork/portzero/releases/latest/download/version.json"
        );
        assert!(url.contains("PortZeroNetwork/portzero"));
        assert!(url.contains("/latest/download/version.json"));
    }

    #[test]
    fn test_parse_semver_basic() {
        let v = parse_semver("1.2.3").expect("should parse");
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.prerelease, None);
    }

    #[test]
    fn test_parse_semver_with_v_prefix() {
        let v = parse_semver("v1.2.3").expect("should parse");
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.prerelease, None);
    }

    #[test]
    fn test_parse_semver_with_prerelease() {
        let v = parse_semver("1.2.3-alpha").expect("should parse");
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.prerelease, Some("alpha".to_string()));
    }

    #[test]
    fn test_parse_semver_with_prerelease_and_v_prefix() {
        let v = parse_semver("v1.2.3-rc.1").expect("should parse");
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
        assert_eq!(v.prerelease, Some("rc.1".to_string()));
    }

    #[test]
    fn test_parse_semver_invalid_formats() {
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("1"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver(""), None);
    }

    #[test]
    fn test_version_comparison_patch() {
        let v1 = parse_semver("1.2.3").unwrap();
        let v2 = parse_semver("1.2.4").unwrap();
        assert!(v1 < v2);
        assert!(v2 > v1);
    }

    #[test]
    fn test_version_comparison_minor() {
        let v1 = parse_semver("1.2.0").unwrap();
        let v2 = parse_semver("1.3.0").unwrap();
        assert!(v1 < v2);
    }

    #[test]
    fn test_version_comparison_major() {
        let v1 = parse_semver("1.0.0").unwrap();
        let v2 = parse_semver("2.0.0").unwrap();
        assert!(v1 < v2);
    }

    #[test]
    fn test_version_comparison_prerelease_vs_release() {
        let prerelease = parse_semver("1.2.3-alpha").unwrap();
        let release = parse_semver("1.2.3").unwrap();
        // Release is greater than prerelease with same version
        assert!(prerelease < release);
        assert!(release > prerelease);
    }

    #[test]
    fn test_version_comparison_prerelease_numeric() {
        let v1 = parse_semver("1.2.3-1").unwrap();
        let v2 = parse_semver("1.2.3-2").unwrap();
        assert!(v1 < v2);
    }

    #[test]
    fn test_version_comparison_prerelease_string() {
        let v1 = parse_semver("1.2.3-alpha").unwrap();
        let v2 = parse_semver("1.2.3-beta").unwrap();
        // Lexicographic comparison: "alpha" < "beta"
        assert!(v1 < v2);
    }

    #[test]
    fn test_version_comparison_prerelease_mixed() {
        let v1 = parse_semver("1.2.3-1.alpha").unwrap();
        let v2 = parse_semver("1.2.3-2.beta").unwrap();
        // Numeric parts are compared numerically: 1 < 2
        assert!(v1 < v2);
    }

    #[test]
    fn test_version_comparison_equality() {
        let v1 = parse_semver("1.2.3").unwrap();
        let v2 = parse_semver("1.2.3").unwrap();
        assert_eq!(v1, v2);
        assert!(v1 >= v2);
        assert!(v1 <= v2);
    }

    #[test]
    fn test_should_check_env_var_disables() {
        std::env::set_var("PZ_TUNNEL_NO_UPDATE_CHECK", "1");
        assert!(!should_check());
        std::env::remove_var("PZ_TUNNEL_NO_UPDATE_CHECK");
    }

    #[test]
    fn test_should_check_missing_timestamp_file() {
        std::env::remove_var("PZ_TUNNEL_NO_UPDATE_CHECK");
        // When timestamp file doesn't exist, should_check returns true
        // (we should check since we've never checked before)
        assert!(should_check());
    }
}
