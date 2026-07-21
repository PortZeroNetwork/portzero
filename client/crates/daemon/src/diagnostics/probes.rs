//! Active DNS and HTTP probes for `portzero.local` and per-tunnel domains, plus
//! the small IP-formatting helpers they share.

use std::error::Error;
use std::net::IpAddr;
use std::path::Path;

use reqwest::StatusCode;
use tokio::net::lookup_host;
use tokio::time::timeout;

use super::{
    Diagnostic, Fix, FixKind, Severity, ACTIVE_PROBE_TIMEOUT, PORTZERO_LOCAL_DASHBOARD_IP,
    PORTZERO_LOCAL_DASHBOARD_IPV4, PORTZERO_LOCAL_HTTP_URL, TUNNEL_DNS_PROBE_CAP,
};

pub(super) async fn probe_portzero_local_dns() -> Diagnostic {
    match timeout(ACTIVE_PROBE_TIMEOUT, lookup_host(("portzero.local", 443))).await {
        Err(_) => Diagnostic {
            id: "dns_probe_timeout".into(),
            severity: Severity::Warning,
            category: "dns".into(),
            title: "Active DNS probe timed out for portzero.local".to_string(),
            detail:
                "The OS resolver did not return an address for portzero.local within 3 seconds."
                    .to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description:
                    "Check the scoped portzero.local resolver or the expected /etc/hosts pin."
                        .to_string(),
                command: None,
            }),
        },
        Ok(Err(err)) => Diagnostic {
            id: "dns_probe_failed".into(),
            severity: Severity::Error,
            category: "dns".into(),
            title: "portzero.local did not resolve".to_string(),
            detail: format!("Active DNS probe failed: {err}"),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description:
                    "Check the scoped portzero.local resolver or the expected /etc/hosts pin."
                        .to_string(),
                command: None,
            }),
        },
        Ok(Ok(addrs)) => {
            let ips = unique_ips(addrs.map(|addr| addr.ip()));
            if ips.contains(&IpAddr::V4(PORTZERO_LOCAL_DASHBOARD_IPV4)) {
                Diagnostic {
                    id: "dns_probe_ok".into(),
                    severity: Severity::Info,
                    category: "dns".into(),
                    title: "portzero.local resolution works".to_string(),
                    detail: format!(
                        "Active DNS probe resolved portzero.local to {}.",
                        format_ip_list(&ips)
                    ),
                    fix: None,
                }
            } else {
                Diagnostic {
                    id: "dns_probe_wrong_target".into(),
                    severity: Severity::Warning,
                    category: "dns".into(),
                    title: "portzero.local resolved to an unexpected address".to_string(),
                    detail: format!(
                        "Active DNS probe resolved portzero.local to {} instead of {}.",
                        format_ip_list(&ips),
                        PORTZERO_LOCAL_DASHBOARD_IP
                    ),
                    fix: Some(Fix {
                        kind: FixKind::Manual,
                        description: "Check for conflicting resolver rules or /etc/hosts entries."
                            .to_string(),
                        command: None,
                    }),
                }
            }
        }
    }
}

/// Probes the local daemon's management HTTP API at `portzero.local`. This is
/// no longer just "the old browser dashboard" — the PortZero desktop app
/// (`portzero-app`) reads all of its data through this same endpoint (see
/// `client/crates/app/src/core.rs::LOCAL_BASE`), so a failure here means the
/// desktop app is (or will be) unable to show tunnel/daemon status, not just
/// that a browser tab would fail to load.
pub(super) async fn probe_portzero_local_http() -> Diagnostic {
    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(ACTIVE_PROBE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            return Diagnostic {
                id: "http_probe_unavailable".into(),
                severity: Severity::Warning,
                category: "network".into(),
                title: "Could not initialize the local daemon API probe".to_string(),
                detail: format!("Failed to build the HTTP probe client: {err}"),
                fix: None,
            }
        }
    };

    match client.get(PORTZERO_LOCAL_HTTP_URL).send().await {
        Ok(response) if response.status() == StatusCode::OK => Diagnostic {
            id: "http_probe_ok".into(),
            severity: Severity::Info,
            category: "network".into(),
            title: "Local daemon API reachable at portzero.local".to_string(),
            detail: format!(
                "Active HTTP probe fetched {} successfully.",
                PORTZERO_LOCAL_HTTP_URL
            ),
            fix: None,
        },
        Ok(response) => Diagnostic {
            id: "http_probe_bad_status".into(),
            severity: Severity::Warning,
            category: "network".into(),
            title: "Local daemon API reachable but returned an unexpected response".to_string(),
            detail: format!(
                "Active HTTP probe fetched {} but received HTTP {}.",
                PORTZERO_LOCAL_HTTP_URL,
                response.status()
            ),
            fix: None,
        },
        Err(err) => Diagnostic {
            id: "http_probe_failed".into(),
            severity: Severity::Warning,
            category: "network".into(),
            title: "Could not reach the local daemon API at portzero.local".to_string(),
            detail: format!(
                "Active HTTP probe to {} failed: {}. This endpoint is what the PortZero \
                 desktop app itself reads for tunnel/daemon status, so this is not limited to \
                 the legacy browser dashboard.",
                PORTZERO_LOCAL_HTTP_URL,
                summarize_reqwest_error(&err)
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Check that the overlay network is active and the daemon is \
                    reachable over HTTP. Run `portzero doctor` for a full diagnosis."
                    .to_string(),
                command: Some("portzero doctor".to_string()),
            }),
        },
    }
}

/// Returns true if `ip` falls inside the `10.254.0.0/16` overlay VIP range
/// (the dashboard itself lives at `10.254.0.2`). IPv6 addresses are never
/// part of the overlay range.
pub(super) fn ip_in_overlay_range(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.octets()[0] == 10 && v4.octets()[1] == 254,
        IpAddr::V6(_) => false,
    }
}

/// Outcome of an active DNS lookup for a single tunnel domain, decoupled from
/// the real I/O in [`lookup_tunnel_domain`] so the classification below can be
/// unit tested without performing a real DNS lookup.
pub(super) enum TunnelLookupResult {
    TimedOut,
    Failed(String),
    Resolved(Vec<IpAddr>),
}

/// Classify the outcome of resolving a `.portzero.local` overlay tunnel
/// domain into a `Diagnostic`, or `None` if it resolved healthily.
pub(super) fn classify_overlay_tunnel_dns(
    domain: &str,
    result: &TunnelLookupResult,
) -> Option<Diagnostic> {
    match result {
        TunnelLookupResult::TimedOut | TunnelLookupResult::Failed(_) => Some(Diagnostic {
            id: "tunnel_dns_failed".into(),
            severity: Severity::Warning,
            category: "dns".into(),
            title: format!("{domain} is not resolving"),
            detail: format!(
                "The local tunnel name {domain} did not resolve to an overlay address. {}",
                lookup_failure_reason(result)
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Run `portzero doctor` or check the scoped .portzero.local resolver."
                    .to_string(),
                command: Some("portzero doctor".to_string()),
            }),
        }),
        TunnelLookupResult::Resolved(ips) => {
            if ips.iter().any(|ip| ip_in_overlay_range(*ip)) {
                None
            } else {
                Some(Diagnostic {
                    id: "tunnel_dns_wrong_target".into(),
                    severity: Severity::Warning,
                    category: "dns".into(),
                    title: format!("{domain} resolved to an unexpected address"),
                    detail: format!(
                        "The local tunnel name {domain} resolved to {} instead of an address in 10.254.0.0/16.",
                        format_ip_list(ips)
                    ),
                    fix: Some(Fix {
                        kind: FixKind::Manual,
                        description:
                            "Check for conflicting resolver rules or /etc/hosts entries overriding this tunnel name."
                                .to_string(),
                        command: None,
                    }),
                })
            }
        }
    }
}

/// Classify the outcome of resolving a `*.tunnel.portzero.cloud` cloud tunnel
/// domain into a `Diagnostic`, or `None` if it resolved.
pub(super) fn classify_cloud_tunnel_dns(
    domain: &str,
    result: &TunnelLookupResult,
) -> Option<Diagnostic> {
    match result {
        TunnelLookupResult::TimedOut | TunnelLookupResult::Failed(_) => Some(Diagnostic {
            id: "cloud_tunnel_dns_failed".into(),
            severity: Severity::Warning,
            category: "dns".into(),
            title: format!("{domain} is not resolving"),
            detail: format!(
                "The public DNS name for cloud tunnel {domain} did not resolve. This can be a \
                 propagation delay after the tunnel was created, or the tunnel may be offline. {}",
                lookup_failure_reason(result)
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description:
                    "Check internet connectivity and confirm the tunnel is registered with portzero.cloud."
                        .to_string(),
                command: None,
            }),
        }),
        TunnelLookupResult::Resolved(_) => None,
    }
}

/// Render the reason a [`TunnelLookupResult`] failed, for use in diagnostic
/// detail messages. Panics if called on `Resolved` (callers only reach this
/// from the failure arms).
fn lookup_failure_reason(result: &TunnelLookupResult) -> String {
    match result {
        TunnelLookupResult::TimedOut => "The active DNS probe timed out.".to_string(),
        TunnelLookupResult::Failed(err) => format!("The active DNS probe failed: {err}"),
        TunnelLookupResult::Resolved(_) => unreachable!("only called from failure arms"),
    }
}

/// Determine which overlay and cloud tunnel domains should be probed, applying
/// the "overlay must be active" gate and the per-list probe cap. Pure and
/// state-free beyond its inputs, so it is unit-testable without touching disk.
pub(super) fn collect_tunnel_probe_domains(
    overlay: &crate::route_table::OverlayState,
    routes: &crate::route_table::RouteTable,
) -> (Vec<String>, Vec<String>) {
    // Skip local overlay tunnels entirely while the overlay is inactive: every
    // lookup would fail and that's already a separate, already-reported
    // condition — probing here would just be per-tunnel noise on top of it.
    let overlay_domains = if overlay.overlay_active {
        let mut domains: Vec<String> = overlay.routes.iter().map(|r| r.domain.clone()).collect();
        domains.sort();
        domains.dedup();
        domains.truncate(TUNNEL_DNS_PROBE_CAP);
        domains
    } else {
        Vec::new()
    };

    let mut cloud_domains: Vec<String> = routes.routes.values().map(|r| r.domain.clone()).collect();
    cloud_domains.sort();
    cloud_domains.dedup();
    cloud_domains.truncate(TUNNEL_DNS_PROBE_CAP);

    (overlay_domains, cloud_domains)
}

async fn lookup_tunnel_domain(domain: &str) -> TunnelLookupResult {
    match timeout(ACTIVE_PROBE_TIMEOUT, lookup_host((domain, 443))).await {
        Err(_) => TunnelLookupResult::TimedOut,
        Ok(Err(err)) => TunnelLookupResult::Failed(err.to_string()),
        Ok(Ok(addrs)) => TunnelLookupResult::Resolved(unique_ips(addrs.map(|addr| addr.ip()))),
    }
}

/// Probe DNS resolution for individual tunnel domains: local
/// `.portzero.local` overlay tunnels and cloud `*.tunnel.portzero.cloud`
/// tunnels. Resolution failure for `portzero.local` itself is already
/// handled by [`probe_portzero_local_dns`]; this only covers per-tunnel
/// domains, and stays quiet for tunnels that resolve healthily to avoid
/// per-tunnel Info spam.
///
/// Probes run concurrently. `run_diagnostics` is awaited inline on the daemon's
/// discovery loop every ~30s, so bounding this to a single timeout (rather than
/// the sum of every tunnel's timeout) keeps a batch of slow/absent tunnel DNS
/// from stalling discovery. Each list is still capped at [`TUNNEL_DNS_PROBE_CAP`].
pub(super) async fn probe_tunnel_dns(state_dir: &Path) -> Vec<Diagnostic> {
    let overlay =
        crate::route_table::OverlayState::load(&state_dir.join("overlay.json")).unwrap_or_default();
    let routes =
        crate::route_table::RouteTable::load(&state_dir.join("routes.json")).unwrap_or_default();

    let (overlay_domains, cloud_domains) = collect_tunnel_probe_domains(&overlay, &routes);

    let overlay_probes = overlay_domains.iter().map(|domain| async move {
        classify_overlay_tunnel_dns(domain, &lookup_tunnel_domain(domain).await)
    });
    let cloud_probes = cloud_domains.iter().map(|domain| async move {
        classify_cloud_tunnel_dns(domain, &lookup_tunnel_domain(domain).await)
    });

    let mut issues: Vec<Diagnostic> = futures_util::future::join_all(overlay_probes)
        .await
        .into_iter()
        .chain(futures_util::future::join_all(cloud_probes).await)
        .flatten()
        .collect();

    issues.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.title.cmp(&b.title))
    });

    issues
}

pub(super) fn unique_ips(ips: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    let mut unique = Vec::new();
    for ip in ips {
        if !unique.contains(&ip) {
            unique.push(ip);
        }
    }
    unique
}

pub(super) fn format_ip_list(ips: &[IpAddr]) -> String {
    ips.iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn summarize_reqwest_error(err: &reqwest::Error) -> String {
    let mut parts = Vec::new();
    parts.push(err.to_string());

    let mut source = err.source();
    while let Some(next) = source {
        let text = next.to_string();
        if !parts.iter().any(|existing| existing == &text) {
            parts.push(text);
        }
        source = next.source();
    }

    parts.join(": ")
}
