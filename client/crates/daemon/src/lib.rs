pub mod auth;
pub mod auto_open;
pub mod autostart;
pub mod browser;
pub mod claims;
pub mod cloud;
pub mod diagnostics;
pub mod discovery;
pub mod discovery_loop;
pub mod docker_events;
pub mod forwarder;
pub mod hosts;
pub mod legacy_monitor;
pub mod management;
pub mod net;
pub mod notify;
pub mod observations;
pub mod protocol_detect;
pub mod route_table;
pub mod secure_file;
pub mod tls;
pub mod versions;

/// Install rustls' process-wide crypto provider.
///
/// Some client-side TLS users, including websocket and HTTP clients, ask rustls
/// for the process default provider. The daemon's server TLS path builds with an
/// explicit provider, but cloud connections need this initialized before first
/// use.
pub fn install_default_crypto_provider() {
    static INIT: std::sync::Once = std::sync::Once::new();

    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Detect whether we are running as the Windows installer service account.
///
/// MSI deferred custom actions can run as `SYSTEM`, which is the wrong context
/// for user-scoped setup steps like CurrentUser trust-store updates or per-user
/// scheduled tasks. Those steps should be skipped rather than failing the
/// installer.
#[cfg(target_os = "windows")]
pub fn is_windows_system_account() -> bool {
    let username_is_system = std::env::var("USERNAME")
        .map(|u| u.eq_ignore_ascii_case("SYSTEM"))
        .unwrap_or(false);
    let profile_is_system = std::env::var("USERPROFILE")
        .map(|p| p.eq_ignore_ascii_case(r"C:\Windows\System32\config\systemprofile"))
        .unwrap_or(false);

    username_is_system || profile_is_system
}

#[cfg(not(target_os = "windows"))]
pub fn is_windows_system_account() -> bool {
    false
}
