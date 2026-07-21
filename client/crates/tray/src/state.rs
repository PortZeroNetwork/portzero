//! Read the daemon's on-disk state into a single snapshot the tray renders.
//!
//! Everything here is file-based (`daemon.pid`, `diagnostics.json`,
//! `issues.json`, `overlay.json`, `routes.json`, `config.toml`) so a snapshot is
//! available even when the daemon is **not** running — which is exactly the case
//! the tray most needs to make visible. The tray never talks to the management
//! HTTP API, because that API is only reachable over the overlay (and therefore
//! only when the daemon is already healthy).

use portzero_daemon::diagnostics::{self, Severity};
use portzero_daemon::discovery_loop::{read_daemon_pid, DaemonConfig};
use portzero_daemon::net::stack::OverlayHttpsPolicy;
pub use portzero_daemon::notify::Problem;
use portzero_daemon::notify::{collect_problems, read_issues};
use portzero_daemon::route_table::{OverlayState, RouteTable};

/// Infrastructure names that live in `overlay.json`-adjacent state but are not
/// user tunnels; never shown in the "Tunnels" list.
const RESERVED_DOMAINS: &[&str] = &["portzero.local", "api.portzero.local"];

/// Overall health, mapped directly to the tray icon colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Daemon up and everything it manages looks good — green.
    Ok,
    /// Daemon up but something needs attention (warnings, issues, an inactive
    /// overlay, or tunnels that don't resolve) — yellow.
    Degraded,
    /// Daemon is down, or a critical problem is present — red.
    Down,
}

/// A single tunnel row for the "Tunnels" submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunnel {
    pub domain: String,
    /// Best-effort browsable URL, e.g. `https://web.myapp.portzero.local`.
    pub url: String,
    /// Whether [`Tunnel::url`] is https (drives the http/https label).
    pub https: bool,
    /// True for cloud (`*.tunnel.portzero.cloud`) tunnels, false for local
    /// `.portzero.local` overlay tunnels.
    pub cloud: bool,
}

/// The full snapshot the tray renders from.
pub struct Snapshot {
    pub running: bool,
    pub health: Health,
    pub tunnels: Vec<Tunnel>,
    pub problems: Vec<Problem>,
    pub https: OverlayHttpsPolicy,
    /// One-line summary for the tray tooltip and disabled header menu item.
    pub summary: String,
}

impl Snapshot {
    /// Read every daemon state file and fold it into a single snapshot.
    pub fn read(config: &DaemonConfig) -> Snapshot {
        let running = read_daemon_pid(config).is_some();

        let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();
        let routes = RouteTable::load(&config.routes_path()).unwrap_or_default();
        // Read the HTTPS policy straight off the passed config. The controller
        // reloads the config on every refresh, so a toggle written to
        // config.toml is reflected on the next tick.
        let https = config.overlay_https;

        let mut tunnels = Vec::new();
        for r in &overlay.routes {
            if RESERVED_DOMAINS.contains(&r.domain.as_str()) {
                continue;
            }
            let (url, is_https) = local_tunnel_url(&r.domain, r.service_port, &https);
            tunnels.push(Tunnel {
                domain: r.domain.clone(),
                url,
                https: is_https,
                cloud: false,
            });
        }
        for r in routes.routes.values() {
            if RESERVED_DOMAINS.contains(&r.domain.as_str()) {
                continue;
            }
            tunnels.push(Tunnel {
                domain: r.domain.clone(),
                url: format!("https://{}", r.domain),
                https: true,
                cloud: true,
            });
        }
        tunnels.sort_by(|a, b| a.domain.cmp(&b.domain));
        tunnels.dedup();

        // issues.json + diagnostics.json, flattened into one ranked list — the
        // same list the dashboard and `/status.json` render, so "Issues" means
        // the same thing everywhere.
        let issues = read_issues(&config.issues_path());
        let report = diagnostics::load_report(&config.state_dir);
        let problems = collect_problems(&issues, report.as_ref());

        let critical = count_sev(&problems, Severity::Critical);
        let errors = count_sev(&problems, Severity::Error);
        let warnings = count_sev(&problems, Severity::Warning);
        // The overlay only matters (and can be "inactive") once there is at least
        // one local tunnel that needs it; an all-cloud or empty setup is not
        // degraded just because the TUN device isn't up.
        let has_local_tunnel = tunnels.iter().any(|t| !t.cloud);
        let overlay_needed_but_inactive = has_local_tunnel && !overlay.overlay_active;

        let health = classify_health(
            running,
            critical,
            errors,
            warnings,
            overlay_needed_but_inactive,
        );
        let summary = summarize(running, health, tunnels.len(), problems.len());

        Snapshot {
            running,
            health,
            tunnels,
            problems,
            https,
            summary,
        }
    }
}

fn count_sev(problems: &[Problem], sev: Severity) -> usize {
    problems.iter().filter(|p| p.severity == sev).count()
}

/// Decide the browsable URL and scheme for a local `.portzero.local` overlay
/// tunnel from its virtual service port and the active HTTPS policy. Pure so it
/// can be unit-tested without any daemon state.
fn local_tunnel_url(
    domain: &str,
    service_port: u16,
    policy: &OverlayHttpsPolicy,
) -> (String, bool) {
    match service_port {
        443 => (format!("https://{domain}"), true),
        // A plain-HTTP service on virtual port 80 is browsable over https only
        // when the daemon is configured to terminate TLS for it.
        80 if policy.enable_for_port_80 => (format!("https://{domain}"), true),
        80 => (format!("http://{domain}"), false),
        // Any other virtual port is reached as plaintext http on that port.
        p => (format!("http://{domain}:{p}"), false),
    }
}

/// Map health signals onto the tray colour. Pure and total so the mapping is
/// unit-tested rather than discovered at runtime.
fn classify_health(
    running: bool,
    critical: usize,
    errors: usize,
    warnings: usize,
    overlay_needed_but_inactive: bool,
) -> Health {
    if !running {
        return Health::Down;
    }
    if critical > 0 {
        return Health::Down;
    }
    if errors > 0 || warnings > 0 || overlay_needed_but_inactive {
        return Health::Degraded;
    }
    Health::Ok
}

/// Build the one-line tooltip / header summary.
fn summarize(running: bool, health: Health, tunnels: usize, problems: usize) -> String {
    if !running {
        return "PortZero daemon is not running".to_string();
    }
    let tunnel_word = if tunnels == 1 { "tunnel" } else { "tunnels" };
    match (health, problems) {
        (Health::Ok, _) => format!("PortZero: {tunnels} {tunnel_word}, all healthy"),
        (_, 1) => format!("PortZero: {tunnels} {tunnel_word}, 1 issue"),
        (_, n) => format!("PortZero: {tunnels} {tunnel_word}, {n} issues"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(enable_for_port_80: bool) -> OverlayHttpsPolicy {
        OverlayHttpsPolicy {
            enable_for_port_80,
            redirect_port_80: true,
            passthrough_port_443: true,
        }
    }

    #[test]
    fn port_443_is_https() {
        let (url, https) = local_tunnel_url("web.app.portzero.local", 443, &policy(false));
        assert_eq!(url, "https://web.app.portzero.local");
        assert!(https);
    }

    #[test]
    fn port_80_is_http_unless_https_enabled() {
        let (url, https) = local_tunnel_url("web.app.portzero.local", 80, &policy(false));
        assert_eq!(url, "http://web.app.portzero.local");
        assert!(!https);

        let (url, https) = local_tunnel_url("web.app.portzero.local", 80, &policy(true));
        assert_eq!(url, "https://web.app.portzero.local");
        assert!(https);
    }

    #[test]
    fn other_ports_are_plaintext_with_explicit_port() {
        let (url, https) = local_tunnel_url("db.app.portzero.local", 8080, &policy(true));
        assert_eq!(url, "http://db.app.portzero.local:8080");
        assert!(!https);
    }

    #[test]
    fn down_when_not_running_regardless_of_counts() {
        assert_eq!(classify_health(false, 0, 0, 0, false), Health::Down);
    }

    #[test]
    fn down_on_critical() {
        assert_eq!(classify_health(true, 1, 0, 0, false), Health::Down);
    }

    #[test]
    fn degraded_on_errors_warnings_or_inactive_overlay() {
        assert_eq!(classify_health(true, 0, 1, 0, false), Health::Degraded);
        assert_eq!(classify_health(true, 0, 0, 1, false), Health::Degraded);
        assert_eq!(classify_health(true, 0, 0, 0, true), Health::Degraded);
    }

    #[test]
    fn ok_when_running_and_clean() {
        assert_eq!(classify_health(true, 0, 0, 0, false), Health::Ok);
    }

    #[test]
    fn summary_reflects_state() {
        assert!(summarize(false, Health::Down, 0, 0).contains("not running"));
        assert!(summarize(true, Health::Ok, 1, 0).contains("1 tunnel,"));
        assert!(summarize(true, Health::Ok, 3, 0).contains("3 tunnels"));
        assert!(summarize(true, Health::Degraded, 2, 1).contains("1 issue"));
        assert!(summarize(true, Health::Degraded, 2, 3).contains("3 issues"));
    }
}
