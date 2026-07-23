//! Single source of truth for the portzero.cloud service endpoints the client
//! talks to.
//!
//! Each endpoint has one canonical production default and one `PZ_TUNNEL_*`
//! environment override used to point the client at a non-production backend
//! (local dev, staging). The client has no notion of a "production vs staging
//! environment" beyond which URLs it targets, so this module — not scattered
//! per-crate constants — is where those URLs and their defaults live.

/// Cloud API base URL (`PZ_TUNNEL_API_URL`).
pub const DEFAULT_API_URL: &str = "https://app.portzero.cloud/api";
/// Edge tunnel WebSocket URL (`PZ_TUNNEL_EDGE_URL`).
pub const DEFAULT_EDGE_URL: &str = "wss://edge.portzero.cloud/tunnel";
/// Dashboard base URL (`PZ_TUNNEL_DASHBOARD_URL`).
pub const DEFAULT_DASHBOARD_URL: &str = "https://app.portzero.cloud";
/// Marketing site base URL (`PZ_TUNNEL_WEB_URL`).
pub const DEFAULT_WEB_URL: &str = "https://portzero.cloud";
/// GitHub Releases base URL (`PZ_TUNNEL_RELEASES_URL`).
pub const DEFAULT_RELEASES_URL: &str = "https://github.com/PortZeroNetwork/portzero/releases";

/// Cloud API base URL, honouring `PZ_TUNNEL_API_URL`.
pub fn api_url() -> String {
    std::env::var("PZ_TUNNEL_API_URL").unwrap_or_else(|_| DEFAULT_API_URL.to_string())
}

/// Edge tunnel WebSocket URL, honouring `PZ_TUNNEL_EDGE_URL`.
pub fn edge_url() -> String {
    std::env::var("PZ_TUNNEL_EDGE_URL").unwrap_or_else(|_| DEFAULT_EDGE_URL.to_string())
}

/// Marketing site base URL, honouring `PZ_TUNNEL_WEB_URL`.
pub fn web_url() -> String {
    std::env::var("PZ_TUNNEL_WEB_URL").unwrap_or_else(|_| DEFAULT_WEB_URL.to_string())
}

/// GitHub Releases page base URL, honouring `PZ_TUNNEL_RELEASES_URL`.
///
/// Used both for user-facing "see the releases page" links and as the root of
/// the update-download base below.
pub fn releases_url() -> String {
    std::env::var("PZ_TUNNEL_RELEASES_URL").unwrap_or_else(|_| DEFAULT_RELEASES_URL.to_string())
}

/// Base location the update checker and self-updater fetch `version.json` and
/// the platform release archive from.
///
/// Defaults to `<releases_url>/latest/download`, which GitHub always resolves
/// to the newest **stable** release (it ignores prereleases). `PZ_TUNNEL_UPDATE_BASE_URL`
/// overrides the whole base so tests can point the updater at a throwaway HTTP
/// server or a local directory (`file://…` or an absolute path) and exercise the
/// download → extract → replace path deterministically and offline.
pub fn update_download_base() -> String {
    if let Ok(base) = std::env::var("PZ_TUNNEL_UPDATE_BASE_URL") {
        return base;
    }
    format!("{}/latest/download", releases_url())
}

/// Dashboard base URL.
///
/// - If `PZ_TUNNEL_DASHBOARD_URL` is set, use it directly.
/// - Else if the API URL is a localhost address, use its host on port 3003.
/// - Otherwise the production dashboard.
pub fn dashboard_url() -> String {
    if let Ok(url) = std::env::var("PZ_TUNNEL_DASHBOARD_URL") {
        return url;
    }
    dashboard_url_from_api_url(&api_url())
}

/// Derive the dashboard URL from an API URL (see [`dashboard_url`]).
pub fn dashboard_url_from_api_url(api_url: &str) -> String {
    if api_url.contains("localhost") || api_url.contains("127.0.0.1") {
        if let Some(colon_pos) = api_url.rfind(':') {
            let base = &api_url[..colon_pos];
            return format!("{base}:3003");
        }
    }
    DEFAULT_DASHBOARD_URL.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_from_localhost_api_uses_3003() {
        assert_eq!(
            dashboard_url_from_api_url("http://localhost:3001"),
            "http://localhost:3003"
        );
        assert_eq!(
            dashboard_url_from_api_url("http://127.0.0.1:3001"),
            "http://127.0.0.1:3003"
        );
    }

    #[test]
    fn dashboard_from_production_api_uses_default() {
        assert_eq!(
            dashboard_url_from_api_url(DEFAULT_API_URL),
            DEFAULT_DASHBOARD_URL
        );
    }

    #[test]
    fn default_releases_url_points_at_current_repo() {
        // The update check + self-updater both resolve through this. A repo/org
        // rename that misses this constant silently strands every installed
        // client's updater, so pin it explicitly.
        assert!(DEFAULT_RELEASES_URL.contains("PortZeroNetwork/portzero"));
        assert!(!DEFAULT_RELEASES_URL.contains("LoumTechnologies"));
        assert!(!DEFAULT_RELEASES_URL.contains("port-zero"));
    }

    #[test]
    fn update_download_base_defaults_to_latest_download() {
        // Guard the environment is clean for a deterministic default.
        std::env::remove_var("PZ_TUNNEL_UPDATE_BASE_URL");
        std::env::remove_var("PZ_TUNNEL_RELEASES_URL");
        assert_eq!(
            update_download_base(),
            "https://github.com/PortZeroNetwork/portzero/releases/latest/download"
        );
    }

    #[test]
    fn update_download_base_honours_override() {
        std::env::set_var("PZ_TUNNEL_UPDATE_BASE_URL", "file:///tmp/pz-fake-release");
        assert_eq!(update_download_base(), "file:///tmp/pz-fake-release");
        std::env::remove_var("PZ_TUNNEL_UPDATE_BASE_URL");
    }
}
