//! Diagnostics: runs health checks across the Binary, Network, DNS, TLS, Auth,
//! and System categories and produces a serializable report. The individual
//! checks live in [`checks`], the active DNS/HTTP probes in [`probes`].

use std::net::Ipv4Addr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::time::Duration;

mod agent_mcp;
mod checks;
mod checks_tls_trust;
mod checks_tray;
pub use checks_tray::{
    tray_missing_detail, tray_presence, tray_start_command, TrayPresence, TRAY_MISSING_FIX,
};
mod probes;

const PORTZERO_LOCAL_DASHBOARD_IP: &str = "10.254.0.2";
const PORTZERO_LOCAL_DASHBOARD_IPV4: Ipv4Addr = Ipv4Addr::new(10, 254, 0, 2);
const PORTZERO_LOCAL_HTTP_URL: &str = "http://portzero.local/status.json";
const ACTIVE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum number of tunnel domains probed per `run_diagnostics` pass. Overlay
/// and cloud tunnels are each capped separately; if a user has more than this
/// many tunnels, only the first N (sorted, for determinism) are probed.
const TUNNEL_DNS_PROBE_CAP: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixKind {
    Auto,
    Confirm,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fix {
    pub kind: FixKind,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub id: String,
    pub severity: Severity,
    pub category: String,
    pub title: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Fix>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiagnosticsReport {
    pub generated_at: String, // RFC3339 via chrono
    pub checks_run: usize,
    pub issues: Vec<Diagnostic>, // sorted: Critical first, then Error, Warning, Info
}
/// Run all diagnostic checks and return a report.
///
/// Blocking I/O is executed via [`tokio::task::spawn_blocking`] to keep the
/// async executor free.
pub async fn run_diagnostics(state_dir: &std::path::Path) -> DiagnosticsReport {
    let state_dir: PathBuf = state_dir.to_path_buf();
    let state_dir_for_blocking = state_dir.clone();

    let (mut issues, mut checks_run) = tokio::task::spawn_blocking(move || {
        let state_dir = state_dir_for_blocking;
        let mut out: Vec<Diagnostic> = Vec::new();
        let mut checks_run: usize = 0;

        macro_rules! run {
            ($check:expr) => {{
                checks_run += 1;
                if let Some(d) = $check {
                    out.push(d);
                }
            }};
        }

        run!(checks::check_binary_exists());
        run!(checks::check_multiple_instances());
        run!(checks::check_state_dir_writable(&state_dir));
        run!(checks::check_state_files_valid(&state_dir));
        run!(checks::check_fd_limit());
        run!(checks::check_etc_hosts_override());
        run!(checks::check_dev_net_tun());
        run!(checks::check_cap_net_admin());
        run!(checks::check_ptrace_scope());
        run!(checks::check_avahi_conflict());
        run!(checks::check_systemd_resolved());
        run!(checks::check_macos_resolver_file());
        run!(checks::check_wintun_present());
        run!(checks::check_dashboard_hosts_pin());
        run!(checks::check_autostart_installed());
        run!(checks_tray::check_tray_running());
        run!(checks::check_conflicting_vpn_software());
        run!(checks::check_auth_token(&state_dir));
        run!(checks::check_cloud_plan(&state_dir));
        run!(checks_tls_trust::check_ca_cert_exists());
        run!(checks_tls_trust::check_local_ca_trust_installation());
        run!(checks_tls_trust::check_snap_brave_tls_trust());
        run!(agent_mcp::check_ai_agent_mcp_registration());

        out.sort_by(|a, b| a.severity.cmp(&b.severity));
        (out, checks_run)
    })
    .await
    .unwrap_or_else(|_| (Vec::new(), 0));

    // Loaded early (rather than only for the tunnel-probe count below) so the
    // HTTP probe can gate on it: right after the daemon starts, DNS answers
    // portzero.local immediately (the embedded DNS server always knows the
    // fixed dashboard VIP), but routing to that VIP requires the overlay/TUN
    // device to actually be up, which lags DNS by a moment. Probing HTTP
    // before the overlay is active is a guaranteed, meaningless failure.
    let overlay_for_count =
        crate::route_table::OverlayState::load(&state_dir.join("overlay.json")).unwrap_or_default();
    let routes_for_count =
        crate::route_table::RouteTable::load(&state_dir.join("routes.json")).unwrap_or_default();

    let dns_probe = probes::probe_portzero_local_dns().await;
    checks_run += 1;
    let dns_probe_ok = dns_probe.id == "dns_probe_ok";
    issues.push(dns_probe);

    checks_run += 1;
    if dns_probe_ok && overlay_for_count.overlay_active {
        issues.push(probes::probe_portzero_local_http().await);
    } else {
        let reason = if !dns_probe_ok {
            "portzero.local did not resolve to the expected address"
        } else {
            "the local overlay network is not active yet"
        };
        issues.push(Diagnostic {
            id: "http_probe_skipped".into(),
            severity: Severity::Info,
            category: "network".into(),
            title: "HTTP probe of the local daemon API skipped".to_string(),
            detail: format!(
                "The active HTTP probe of {PORTZERO_LOCAL_HTTP_URL} did not run because {reason}."
            ),
            fix: None,
        });
    }

    // Determine how many per-tunnel probes will actually run so `checks_run`
    // reflects reality, then run them. Reuses the overlay/route state already
    // loaded above (matches the existing pattern of `check_state_files_valid`
    // also reading routes.json/overlay.json independently).
    let (overlay_domains, cloud_domains) =
        probes::collect_tunnel_probe_domains(&overlay_for_count, &routes_for_count);
    checks_run += overlay_domains.len() + cloud_domains.len();

    issues.extend(probes::probe_tunnel_dns(&state_dir).await);

    issues.sort_by(|a, b| a.severity.cmp(&b.severity));

    DiagnosticsReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        checks_run,
        issues,
    }
}

/// Persist a diagnostics report to `<state_dir>/diagnostics.json`.
pub fn save_report(report: &DiagnosticsReport, state_dir: &std::path::Path) {
    let path = state_dir.join("diagnostics.json");
    match serde_json::to_string_pretty(report) {
        Ok(json) => {
            let _ = std::fs::write(&path, json);
        }
        Err(e) => tracing::warn!("failed to serialize diagnostics: {e}"),
    }
}

/// Load the most recently persisted diagnostics report from `<state_dir>/diagnostics.json`.
pub fn load_report(state_dir: &std::path::Path) -> Option<DiagnosticsReport> {
    let content = std::fs::read_to_string(state_dir.join("diagnostics.json")).ok()?;
    serde_json::from_str(&content).ok()
}

/// Diagnostic IDs that represent a regression of a post-setup step: something
/// `portzero setup` or the installers configured that has since been removed or
/// broken out-of-band. These are the checks the daemon turns into a one-shot
/// desktop notification (see `discovery_loop::notify_post_setup_regressions`),
/// in addition to surfacing them in `portzero status`.
///
/// `macos_resolver_missing` is deliberately excluded: the overlay self-heals the
/// scoped resolver and fires its own notification, so including it here would
/// double-notify.
const POST_SETUP_REGRESSION_IDS: &[&str] = &[
    "ca_cert_missing",
    "ca_trust_missing",
    "cap_net_admin_missing",
    "systemd_resolved_not_running",
    "wintun_missing",
    "dev_net_tun_missing",
    "dashboard_hosts_pin_missing",
    "autostart_missing",
];

/// Whether a diagnostic `id` is a post-setup regression worth a desktop
/// notification (as opposed to an informational or environmental finding that
/// only belongs in `portzero status`).
pub fn is_post_setup_regression(id: &str) -> bool {
    POST_SETUP_REGRESSION_IDS.contains(&id)
}

#[cfg(test)]
mod tests {
    use super::checks::{analyze_portzero_hosts_entries, nsswitch_prefers_mdns_for_local_hosts};
    use super::probes::{
        classify_cloud_tunnel_dns, classify_overlay_tunnel_dns, collect_tunnel_probe_domains,
        format_ip_list, ip_in_overlay_range, unique_ips, TunnelLookupResult,
    };
    use crate::discovery::ServiceSource;
    use crate::route_table::{OverlayRoute, OverlayState, Route, RouteTable};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn overlay_route(domain: &str) -> OverlayRoute {
        OverlayRoute {
            domain: domain.to_string(),
            domain_template: String::new(),
            substitutions: Default::default(),
            service_port: 8080,
            backend_protocol: None,
            real_addr: "127.0.0.1:32771".to_string(),
            health_path: None,
            pid: 1234,
            source: ServiceSource::Process { cwd: None },
        }
    }

    fn cloud_route(domain: &str) -> Route {
        Route {
            domain: domain.to_string(),
            domain_template: String::new(),
            substitutions: Default::default(),
            host: "127.0.0.1".to_string(),
            port: 8080,
            extra_ports: Vec::new(),
            health_path: None,
            source: ServiceSource::Process { cwd: None },
            pid: 1234,
            discovered_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn expected_linux_dashboard_pin_is_not_treated_as_conflict() {
        let analysis = analyze_portzero_hosts_entries(
            "127.0.0.1 localhost\n10.254.0.2 portzero.local # portzero-local\n",
        );

        assert!(analysis.has_expected_dashboard_pin);
        assert!(analysis.conflicting_lines.is_empty());
    }

    #[test]
    fn unexpected_portzero_local_hosts_entry_is_flagged() {
        let analysis =
            analyze_portzero_hosts_entries("127.0.0.1 localhost\n127.0.1.1 portzero.local\n");

        assert!(!analysis.has_expected_dashboard_pin);
        assert_eq!(analysis.conflicting_lines, vec!["127.0.1.1 portzero.local"]);
    }

    #[test]
    fn nsswitch_mdns_notfound_return_is_treated_as_risky_for_local_hosts() {
        assert!(nsswitch_prefers_mdns_for_local_hosts(
            "passwd: files\nhosts: files mdns4_minimal [NOTFOUND=return] dns\n"
        ));
    }

    #[test]
    fn nsswitch_without_mdns_short_circuit_is_not_treated_as_risky() {
        assert!(!nsswitch_prefers_mdns_for_local_hosts(
            "passwd: files\nhosts: files dns\n"
        ));
    }

    #[test]
    fn unique_ips_preserves_order_while_deduplicating() {
        let ips = unique_ips([
            IpAddr::V4(Ipv4Addr::new(10, 254, 0, 2)),
            IpAddr::V4(Ipv4Addr::new(10, 254, 0, 2)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]);

        assert_eq!(
            ips,
            vec![
                IpAddr::V4(Ipv4Addr::new(10, 254, 0, 2)),
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            ]
        );
    }

    #[test]
    fn format_ip_list_joins_addresses_for_probe_messages() {
        let text = format_ip_list(&[
            IpAddr::V4(Ipv4Addr::new(10, 254, 0, 2)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]);

        assert_eq!(text, "10.254.0.2, ::1");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn existing_paths_returns_only_present_paths() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("snap/brave/current");
        let missing = dir.path().join("snap/brave/old");
        std::fs::create_dir_all(&present).unwrap();

        let found = super::checks_tls_trust::existing_paths([present.clone(), missing]);

        assert_eq!(found, vec![present]);
    }

    #[test]
    fn post_setup_regressions_are_classified() {
        // Post-setup steps that can regress out-of-band notify the user.
        for id in [
            "ca_cert_missing",
            "ca_trust_missing",
            "cap_net_admin_missing",
            "systemd_resolved_not_running",
            "wintun_missing",
            "dev_net_tun_missing",
            "dashboard_hosts_pin_missing",
            "autostart_missing",
        ] {
            assert!(
                super::is_post_setup_regression(id),
                "{id} should be a post-setup regression"
            );
        }
    }

    #[test]
    fn non_regressions_are_not_classified() {
        // Informational / environmental / transient findings must NOT notify.
        for id in [
            "vpn_software_detected",
            "auth_not_logged_in",
            "auth_token_expires_soon",
            "multiple_instances",
            "dns_probe_ok",
            "http_probe_skipped",
            "ai_agent_mcp_not_registered",
            // The macOS resolver has its own self-heal + notification path, so it
            // must be excluded here to avoid double-notifying.
            "macos_resolver_missing",
        ] {
            assert!(
                !super::is_post_setup_regression(id),
                "{id} must not be treated as a post-setup regression"
            );
        }
    }

    #[test]
    fn ip_in_overlay_range_accepts_overlay_addresses() {
        assert!(ip_in_overlay_range(IpAddr::V4(Ipv4Addr::new(
            10, 254, 0, 5
        ))));
    }

    #[test]
    fn ip_in_overlay_range_rejects_neighboring_subnet() {
        assert!(!ip_in_overlay_range(IpAddr::V4(Ipv4Addr::new(
            10, 253, 0, 5
        ))));
    }

    #[test]
    fn ip_in_overlay_range_rejects_loopback() {
        assert!(!ip_in_overlay_range(IpAddr::V4(Ipv4Addr::new(
            127, 0, 0, 1
        ))));
    }

    #[test]
    fn ip_in_overlay_range_rejects_ipv6() {
        assert!(!ip_in_overlay_range(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn overlay_tunnel_timeout_is_flagged() {
        let diag =
            classify_overlay_tunnel_dns("web.myapp.portzero.local", &TunnelLookupResult::TimedOut)
                .expect("expected a diagnostic");
        assert_eq!(diag.id, "tunnel_dns_failed");
        assert_eq!(diag.severity, super::Severity::Warning);
    }

    #[test]
    fn overlay_tunnel_error_is_flagged() {
        let diag = classify_overlay_tunnel_dns(
            "web.myapp.portzero.local",
            &TunnelLookupResult::Failed("no such host".to_string()),
        )
        .expect("expected a diagnostic");
        assert_eq!(diag.id, "tunnel_dns_failed");
    }

    #[test]
    fn overlay_tunnel_resolved_in_range_is_healthy() {
        let result = TunnelLookupResult::Resolved(vec![IpAddr::V4(Ipv4Addr::new(10, 254, 0, 9))]);
        assert!(classify_overlay_tunnel_dns("web.myapp.portzero.local", &result).is_none());
    }

    #[test]
    fn overlay_tunnel_resolved_outside_range_is_flagged() {
        let result = TunnelLookupResult::Resolved(vec![IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))]);
        let diag = classify_overlay_tunnel_dns("web.myapp.portzero.local", &result)
            .expect("expected a diagnostic");
        assert_eq!(diag.id, "tunnel_dns_wrong_target");
        assert!(diag.detail.contains("1.2.3.4"));
    }

    #[test]
    fn cloud_tunnel_timeout_is_flagged() {
        let diag = classify_cloud_tunnel_dns(
            "api.alice.tunnel.portzero.cloud",
            &TunnelLookupResult::TimedOut,
        )
        .expect("expected a diagnostic");
        assert_eq!(diag.id, "cloud_tunnel_dns_failed");
        assert_eq!(diag.severity, super::Severity::Warning);
    }

    #[test]
    fn cloud_tunnel_resolved_is_healthy() {
        let result = TunnelLookupResult::Resolved(vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))]);
        assert!(classify_cloud_tunnel_dns("api.alice.tunnel.portzero.cloud", &result).is_none());
    }

    #[test]
    fn collect_tunnel_probe_domains_skips_overlay_when_inactive() {
        let overlay = OverlayState {
            overlay_active: false,
            routes: vec![overlay_route("web.myapp.portzero.local")],
        };
        let routes = RouteTable::default();

        let (overlay_domains, cloud_domains) = collect_tunnel_probe_domains(&overlay, &routes);
        assert!(overlay_domains.is_empty());
        assert!(cloud_domains.is_empty());
    }

    #[test]
    fn collect_tunnel_probe_domains_includes_active_overlay_routes() {
        let overlay = OverlayState {
            overlay_active: true,
            routes: vec![
                overlay_route("web.myapp.portzero.local"),
                overlay_route("api.myapp.portzero.local"),
            ],
        };
        let routes = RouteTable::default();

        let (overlay_domains, cloud_domains) = collect_tunnel_probe_domains(&overlay, &routes);
        assert_eq!(
            overlay_domains,
            vec![
                "api.myapp.portzero.local".to_string(),
                "web.myapp.portzero.local".to_string(),
            ]
        );
        assert!(cloud_domains.is_empty());
    }

    #[test]
    fn collect_tunnel_probe_domains_dedups_and_sorts_cloud_routes() {
        let overlay = OverlayState::default();
        let mut routes = RouteTable::default();
        routes.routes.insert(
            "k1".to_string(),
            cloud_route("z.alice.tunnel.portzero.cloud"),
        );
        routes.routes.insert(
            "k2".to_string(),
            cloud_route("a.alice.tunnel.portzero.cloud"),
        );

        let (overlay_domains, cloud_domains) = collect_tunnel_probe_domains(&overlay, &routes);
        assert!(overlay_domains.is_empty());
        assert_eq!(
            cloud_domains,
            vec![
                "a.alice.tunnel.portzero.cloud".to_string(),
                "z.alice.tunnel.portzero.cloud".to_string(),
            ]
        );
    }

    #[test]
    fn collect_tunnel_probe_domains_caps_at_limit() {
        let overlay_routes: Vec<OverlayRoute> = (0..30)
            .map(|i| overlay_route(&format!("svc{i:02}.myapp.portzero.local")))
            .collect();
        let overlay = OverlayState {
            overlay_active: true,
            routes: overlay_routes,
        };
        let routes = RouteTable::default();

        let (overlay_domains, _cloud_domains) = collect_tunnel_probe_domains(&overlay, &routes);
        assert_eq!(overlay_domains.len(), super::TUNNEL_DNS_PROBE_CAP);
    }
}
