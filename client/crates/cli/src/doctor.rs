//! `portzero doctor`: one-shot diagnostics for the overlay + DNS + TLS path.
//!
//! Motivated by the #1 first-run failure: the daemon reports "running" while the
//! overlay is actually inactive (it was started unprivileged, or its startup
//! wedged), so `.portzero.local` names never resolve and users conclude the
//! product is broken. `portzero status` gives no hint; `portzero doctor` runs a
//! series of named checks, prints pass/warn/fail with a concrete fix hint on
//! failure, and exits non-zero if any check fails.
//!
//! Doctor reuses the daemon's existing state/IPC plumbing (PID file, overlay
//! state, cloud state, resolver-config status, trust-store verification) rather
//! than duplicating probes, but is written to still produce a meaningful report
//! when the daemon is NOT running.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::Result;
use tokio::net::{lookup_host, TcpStream, UdpSocket};
use tokio::time::timeout;

use portzero_daemon::discovery_loop::{
    read_cloud_connected, read_cloud_error, read_daemon_pid, DaemonConfig,
};
use portzero_daemon::net::overlay::default_resolver_dns_addr;
use portzero_daemon::net::resolver_config::{self, ResolverStatus};
use portzero_daemon::route_table::{OverlayRoute, OverlayState};
use portzero_daemon::tls::{trust, LocalCa};

use crate::auth::AuthConfig;

mod overlay;

pub(crate) use overlay::check_overlay_active;

/// The dashboard name, pinned to a fixed VIP; the embedded DNS answers it
/// directly (before any service registration), so it doubles as a synthetic DNS
/// probe.
const DASHBOARD_NAME: &str = "portzero.local";
/// The fixed VIP the dashboard name must resolve to (`10.254.0.2`).
const DASHBOARD_VIP: Ipv4Addr = Ipv4Addr::new(10, 254, 0, 2);
/// Overlay virtual IP range: `10.254.0.0/16`.
const OVERLAY_PREFIX: [u8; 2] = [10, 254];

const DNS_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// One check's outcome.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Pass,
    Warn,
    Fail,
}

impl Status {
    fn glyph(self) -> char {
        match self {
            Status::Pass => '✓',
            Status::Warn => '!',
            Status::Fail => '✗',
        }
    }
}

/// A single named diagnostic result.
pub(crate) struct Check {
    name: &'static str,
    status: Status,
    detail: String,
    /// Concrete remediation, printed indented under the check on warn/fail.
    fix: Option<String>,
}

impl Check {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Pass,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    /// Whether this check passed (no warning or failure to surface).
    pub(crate) fn is_pass(&self) -> bool {
        self.status == Status::Pass
    }
}

/// Print one non-passing check in the doctor's `!`/`✗` style, with its
/// `fix:` line. Used by `portzero start` and `portzero demo` to surface the
/// same first-run diagnostics doctor reports, without running the full suite.
pub(crate) fn print_alert(check: &Check) {
    println!(
        "  {}  {}: {}",
        check.status.glyph(),
        check.name,
        check.detail
    );
    if check.status != Status::Pass {
        if let Some(fix) = &check.fix {
            println!("       fix: {fix}");
        }
    }
}

/// Run every check, print an aligned report, and exit non-zero if any failed.
pub async fn run() -> Result<()> {
    let config = DaemonConfig::load();
    let mut checks: Vec<Check> = Vec::new();

    // 1. Daemon running (and PID / uptime).
    let pid = read_daemon_pid(&config);
    checks.push(check_daemon_running(&config, pid));

    // 2. Daemon privileged / overlay active.
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();
    checks.push(check_overlay_active(pid, &overlay));

    // 3. Scoped OS resolver installed for portzero.local.
    let resolver_addr = default_resolver_dns_addr();
    checks.push(check_scoped_resolver(resolver_addr));

    // 4. Embedded DNS server actually answers.
    checks.push(check_embedded_dns(resolver_addr).await);

    // 5. End-to-end resolution through the OS resolver (getaddrinfo).
    checks.push(check_os_resolution().await);

    // 6. Local CA generated and installed in the OS trust store.
    checks.push(check_ca());

    // 7. macOS /etc/hosts dashboard pin.
    #[cfg(target_os = "macos")]
    checks.push(check_hosts_pin());

    // 8. Per-tunnel reachability for currently discovered local tunnels.
    checks.extend(check_tunnels(&overlay).await);

    // 9. Cloud tunnel state (informational when logged out).
    checks.push(check_cloud(&config).await);

    // 10. Every running component built from the same release.
    checks.push(check_component_versions());

    // 11. The system-tray companion is there if the user asked for it.
    checks.push(check_tray());

    print_report(&checks);

    if checks.iter().any(|c| c.status == Status::Fail) {
        std::process::exit(1);
    }
    Ok(())
}

/// Do the running components agree on their version?
///
/// A mismatch means an upgrade replaced the binaries while an older daemon,
/// tray, or app kept running — the components then speak different protocol and
/// state-file versions to each other, which shows up later as confusing,
/// unrelated-looking failures. It is a warning rather than a failure because
/// nothing is broken *yet*.
fn check_component_versions() -> Check {
    let report = crate::version::collect();
    if report.consistent {
        return Check::pass("component versions", report.summary);
    }
    Check::warn(
        "component versions",
        report.summary,
        format!(
            "{} (run `portzero version` for the full breakdown)",
            report
                .next_steps
                .first()
                .map(String::as_str)
                .unwrap_or("restart the PortZero daemon, tray, and app")
        ),
    )
}

/// Is there actually a tray icon?
///
/// "The tray is running" is the one claim a user can check with their own eyes,
/// so it is the one worth getting right. This looks for the process itself
/// rather than trusting `~/.portzero/tray.pid` or the component record — a
/// stale record naming a recycled PID is exactly the failure that let a machine
/// with no tray icon pass every check while reporting the tray as running.
fn check_tray() -> Check {
    use portzero_daemon::diagnostics::{
        tray_missing_detail, tray_presence, tray_start_command, TrayPresence, TRAY_MISSING_FIX,
    };

    match tray_presence() {
        TrayPresence::Running(pid) => Check::pass("tray", format!("running as PID {pid}")),
        TrayPresence::NotConfigured => Check::pass(
            "tray",
            "not set to start at login — the tray is optional and nothing depends on it",
        ),
        TrayPresence::Missing => Check::warn(
            "tray",
            tray_missing_detail(),
            format!("{TRAY_MISSING_FIX}: {}", tray_start_command()),
        ),
    }
}

/// Is the daemon process running? Report its PID and (best-effort) uptime.
fn check_daemon_running(config: &DaemonConfig, pid: Option<u32>) -> Check {
    match pid {
        Some(pid) => {
            let uptime = pid_file_uptime(config)
                .map(|d| format!(", up {}", format_duration(d)))
                .unwrap_or_default();
            Check::pass("daemon running", format!("PID {pid}{uptime}"))
        }
        // The PID file only records the daemon that started last, and it is
        // removed when that PID dies — so a daemon this user cannot manage
        // (typically the root service) reads back as "not running". Saying so
        // is what led the tray and the app to start another one. Distinguish
        // the two cases before reporting.
        None => match portzero_daemon::management::pid_lookup::find_unmanaged_daemon() {
            Some(pid) => Check::fail(
                "daemon running",
                format!(
                    "a daemon is running (PID {pid}) but is not the one recorded in this \
                     user's state file — it was most likely started by the system service, \
                     so this user cannot manage it"
                ),
                "sudo portzero restart",
            ),
            None => Check::fail(
                "daemon running",
                "the daemon is not running",
                "portzero start",
            ),
        },
    }
}

/// Is the scoped OS resolver for `portzero.local` installed?
fn check_scoped_resolver(resolver_addr: SocketAddr) -> Check {
    match resolver_config::status(resolver_addr) {
        ResolverStatus::Present => Check::pass(
            "scoped resolver",
            format!("portzero.local queries route to {resolver_addr}"),
        ),
        ResolverStatus::Missing => Check::fail(
            "scoped resolver",
            "the scoped portzero.local resolver is not installed",
            scoped_resolver_fix_hint(),
        ),
        ResolverStatus::Unknown => Check::warn(
            "scoped resolver",
            "resolver state can't be checked cheaply on this platform (systemd-resolved per-link / Windows NRPT)",
            "if names don't resolve, run: sudo portzero restart",
        ),
    }
}

fn scoped_resolver_fix_hint() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "sudo portzero restart  (recreates /etc/resolver/portzero.local)"
    }
    #[cfg(target_os = "linux")]
    {
        "ensure systemd-resolved (or dnsmasq) is running, then: sudo portzero restart"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "sudo portzero restart"
    }
}

/// Query the embedded DNS server directly and confirm it answers with a VIP
/// in the overlay range. Probing `portzero.local` works even before any service
/// is registered, because the server answers the dashboard name unconditionally.
async fn check_embedded_dns(resolver_addr: SocketAddr) -> Check {
    match probe_embedded_dns(resolver_addr, DASHBOARD_NAME).await {
        Ok(ips) if ips.iter().any(|ip| is_overlay_vip(*ip)) => Check::pass(
            "embedded DNS",
            format!(
                "{resolver_addr} answered {DASHBOARD_NAME} -> {}",
                format_ips(&ips)
            ),
        ),
        Ok(ips) if ips.is_empty() => Check::fail(
            "embedded DNS",
            format!("{resolver_addr} returned no A record for {DASHBOARD_NAME}"),
            "confirm the overlay is active (see the 'overlay active' check above)",
        ),
        Ok(ips) => Check::warn(
            "embedded DNS",
            format!(
                "{DASHBOARD_NAME} answered {} (not in the 10.254.0.0/16 overlay range)",
                format_ips(&ips)
            ),
            "check for another DNS server intercepting portzero.local",
        ),
        Err(e) => Check::fail(
            "embedded DNS",
            format!("no answer from the embedded DNS server at {resolver_addr}: {e}"),
            "confirm the overlay is active (see the 'overlay active' check above)",
        ),
    }
}

/// Resolve the dashboard name through the OS resolver (getaddrinfo). This
/// catches OS-level misrouting the direct embedded-DNS probe cannot: the server
/// answers, but the OS never sends `portzero.local` queries to it.
pub(crate) async fn check_os_resolution() -> Check {
    match timeout(RESOLVE_TIMEOUT, lookup_host((DASHBOARD_NAME, 443))).await {
        Ok(Ok(addrs)) => {
            let ips: Vec<Ipv4Addr> = addrs
                .filter_map(|a| match a.ip() {
                    IpAddr::V4(v4) => Some(v4),
                    IpAddr::V6(_) => None,
                })
                .collect();
            if ips.contains(&DASHBOARD_VIP) {
                Check::pass(
                    "OS resolution",
                    format!("getaddrinfo({DASHBOARD_NAME}) -> {DASHBOARD_VIP}"),
                )
            } else if ips.is_empty() {
                Check::fail(
                    "OS resolution",
                    format!("getaddrinfo({DASHBOARD_NAME}) returned no IPv4 address"),
                    os_resolution_fix_hint(),
                )
            } else {
                Check::warn(
                    "OS resolution",
                    format!(
                        "getaddrinfo({DASHBOARD_NAME}) -> {} instead of {DASHBOARD_VIP}",
                        format_ips(&ips)
                    ),
                    "check /etc/hosts and scoped-resolver rules for a conflicting entry",
                )
            }
        }
        Ok(Err(e)) => Check::fail(
            "OS resolution",
            format!("getaddrinfo({DASHBOARD_NAME}) failed: {e}"),
            os_resolution_fix_hint(),
        ),
        Err(_) => Check::fail(
            "OS resolution",
            format!("getaddrinfo({DASHBOARD_NAME}) timed out — the classic DNS-timeout first-run failure"),
            os_resolution_fix_hint(),
        ),
    }
}

fn os_resolution_fix_hint() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "confirm the overlay is active and the /etc/hosts pin / scoped resolver are present, then: sudo portzero restart"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "confirm the overlay is active and the scoped resolver is installed, then: sudo portzero restart"
    }
}

/// Local CA generated and installed in the OS trust store. Reuses
/// `tls::trust::verify_installation` rather than reimplementing store checks.
fn check_ca() -> Check {
    let ca_path = match LocalCa::ca_cert_path() {
        Ok(p) => p,
        Err(e) => {
            return Check::warn(
                "local CA trust",
                format!("could not determine the CA certificate path: {e}"),
                "portzero trust generate && sudo portzero trust install",
            )
        }
    };

    if !ca_path.exists() {
        return Check::fail(
            "local CA trust",
            "the local CA certificate has not been generated",
            "portzero trust generate && sudo portzero trust install",
        );
    }

    match trust::verify_installation(&ca_path) {
        Ok(report) if report.is_clean() => Check::pass(
            "local CA trust",
            "CA generated and present in the OS trust store",
        ),
        Ok(report) => Check::warn(
            "local CA trust",
            format!(
                "CA generated but missing from: {}",
                report.missing.join(", ")
            ),
            "sudo portzero trust install",
        ),
        Err(e) => Check::warn(
            "local CA trust",
            format!("could not verify the trust store: {e}"),
            "sudo portzero trust install",
        ),
    }
}

/// macOS pins `10.254.0.2 portzero.local` in /etc/hosts because mDNSResponder
/// answers bare `.local` names before the scoped resolver. This is only a
/// warning: if the scoped resolver already resolves the dashboard name, the pin
/// is redundant — but its absence is still the usual cause of a flaky dashboard.
#[cfg(target_os = "macos")]
fn check_hosts_pin() -> Check {
    let content = match std::fs::read_to_string("/etc/hosts") {
        Ok(c) => c,
        Err(e) => {
            return Check::warn(
                "hosts pin",
                format!("could not read /etc/hosts: {e}"),
                "sudo portzero setup",
            )
        }
    };

    match analyze_hosts_pin(&content) {
        HostsPin::Expected => {
            Check::pass("hosts pin", "10.254.0.2 portzero.local present in /etc/hosts")
        }
        HostsPin::Conflicting(line) => Check::warn(
            "hosts pin",
            format!(
                "/etc/hosts maps portzero.local to an unexpected address: {line}{}",
                hosts_write_safety_detail()
            ),
            "keep only `10.254.0.2 portzero.local`, or: sudo portzero setup",
        ),
        HostsPin::Missing => Check::warn(
            "hosts pin",
            format!(
                "the `10.254.0.2 portzero.local` pin is missing; the dashboard name may resolve unreliably{}",
                hosts_write_safety_detail()
            ),
            "sudo portzero setup",
        ),
    }
}

/// Append a note to a hosts-pin warning when `sudo portzero setup`'s edit is
/// expected to fail or not persist, so the fix hint isn't misleading.
#[cfg(target_os = "macos")]
fn hosts_write_safety_detail() -> String {
    let safety =
        portzero_daemon::hosts::check_hosts_write_safety(std::path::Path::new("/etc/hosts"));
    if safety.is_safe() {
        return String::new();
    }
    let mut detail = String::from("\n  `sudo portzero setup` is unlikely to fix this on its own:");
    for blocker in &safety.blockers {
        let (label, explanation) = blocker.describe();
        detail.push_str(&format!("\n    - {label}: {explanation}"));
    }
    detail
}

/// For every discovered local tunnel: confirm its name resolves to a VIP and
/// a TCP connection to VIP:port succeeds.
async fn check_tunnels(overlay: &OverlayState) -> Vec<Check> {
    if overlay.routes.is_empty() {
        return vec![Check::pass(
            "local tunnels",
            "no .portzero.local tunnels discovered yet",
        )];
    }

    let mut out = Vec::new();
    for route in &overlay.routes {
        out.push(check_one_tunnel(route).await);
    }
    out
}

async fn check_one_tunnel(route: &OverlayRoute) -> Check {
    let port = route.service_port;
    let name = leak_name(&route.domain);

    // Resolve the tunnel name through the OS resolver to its VIP.
    let vip = match timeout(RESOLVE_TIMEOUT, lookup_host((route.domain.as_str(), port))).await {
        Ok(Ok(addrs)) => addrs
            .filter_map(|a| match a.ip() {
                IpAddr::V4(v4) if is_overlay_vip(v4) => Some(v4),
                _ => None,
            })
            .next(),
        Ok(Err(_)) | Err(_) => None,
    };

    let Some(vip) = vip else {
        return Check::fail(
            name,
            format!("{} did not resolve to an overlay VIP", route.domain),
            "confirm the overlay is active and the scoped resolver is installed",
        );
    };

    // TCP connect to the VIP:port.
    let target = SocketAddr::new(IpAddr::V4(vip), port);
    match timeout(CONNECT_TIMEOUT, TcpStream::connect(target)).await {
        Ok(Ok(_)) => Check::pass(name, format!("{} -> {target} reachable", route.domain)),
        Ok(Err(e)) => Check::fail(
            name,
            format!(
                "{} resolves to {vip} but TCP connect to :{port} failed: {e}",
                route.domain
            ),
            format!(
                "check that the backend behind {} is still listening",
                route.domain
            ),
        ),
        Err(_) => Check::fail(
            name,
            format!(
                "{} resolves to {vip} but TCP connect to :{port} timed out",
                route.domain
            ),
            format!(
                "check that the backend behind {} is still listening",
                route.domain
            ),
        ),
    }
}

/// Cloud tunnel state. Logged out is informational, never a failure.
async fn check_cloud(config: &DaemonConfig) -> Check {
    if AuthConfig::load().is_err() {
        return Check::pass(
            "cloud tunnel",
            "not logged in — cloud tunnels disabled (run `portzero login` to enable)",
        );
    }

    match read_cloud_connected(config) {
        Some(true) => Check::pass("cloud tunnel", "connected to the cloud edge"),
        Some(false) => {
            let err = read_cloud_error(config).unwrap_or_else(|| "disconnected".to_string());
            let auth_rejected = err.contains("Authentication failed");
            Check::warn(
                "cloud tunnel",
                format!("disconnected — {err}"),
                if auth_rejected {
                    "portzero login"
                } else {
                    "check network connectivity to the cloud edge"
                },
            )
        }
        None => Check::warn(
            "cloud tunnel",
            "connecting (no connection state reported yet)",
            "re-run `portzero doctor` shortly, or check `portzero status`",
        ),
    }
}

fn print_report(checks: &[Check]) {
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);

    println!("portzero doctor\n");
    for c in checks {
        println!(
            "  {}  {:<width$}  {}",
            c.status.glyph(),
            c.name,
            c.detail,
            width = width
        );
        if c.status != Status::Pass {
            if let Some(fix) = &c.fix {
                println!("     {:<width$}  fix: {}", "", fix, width = width);
            }
        }
    }

    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warned = checks.iter().filter(|c| c.status == Status::Warn).count();
    println!();
    match (failed, warned) {
        (0, 0) => println!("All checks passed."),
        (0, w) => println!("{w} warning(s), no failures."),
        (f, 0) => println!("{f} check(s) failed."),
        (f, w) => println!("{f} check(s) failed, {w} warning(s)."),
    }
}

/// Best-effort daemon uptime from the PID file's modification time (written once
/// at startup).
fn pid_file_uptime(config: &DaemonConfig) -> Option<Duration> {
    let modified = std::fs::metadata(config.pid_path()).ok()?.modified().ok()?;
    modified.elapsed().ok()
}

/// Whether `ip` falls in the overlay's `10.254.0.0/16` range.
fn is_overlay_vip(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == OVERLAY_PREFIX[0] && o[1] == OVERLAY_PREFIX[1]
}

fn format_ips(ips: &[Ipv4Addr]) -> String {
    ips.iter()
        .map(|ip| ip.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Human-friendly, coarse duration ("42s", "5m", "2h13m", "3d1h").
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86400, (secs % 86400) / 3600)
    }
}

/// The per-tunnel check name embeds the domain, but `Check::name` is `&'static
/// str`. Leak the small string: `doctor` is a short-lived one-shot command, so
/// the handful of leaked tunnel names are reclaimed at process exit.
fn leak_name(domain: &str) -> &'static str {
    Box::leak(format!("tunnel {domain}").into_boxed_str())
}

#[cfg(target_os = "macos")]
enum HostsPin {
    Expected,
    Conflicting(String),
    Missing,
}

/// Classify how `/etc/hosts` maps `portzero.local`. The dashboard VIP counts as
/// the expected pin; any other address mapping the name is a conflict.
#[cfg(any(target_os = "macos", test))]
fn analyze_hosts_pin(content: &str) -> HostsPin {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let before_comment = trimmed.split('#').next().unwrap_or(trimmed);
        let mut parts = before_comment.split_whitespace();
        let Some(ip) = parts.next() else { continue };
        if !parts.any(|name| name == DASHBOARD_NAME) {
            continue;
        }
        if ip == DASHBOARD_VIP.to_string() {
            return HostsPin::Expected;
        }
        return HostsPin::Conflicting(before_comment.trim().to_string());
    }
    HostsPin::Missing
}

#[cfg(all(test, not(target_os = "macos")))]
enum HostsPin {
    Expected,
    // Only matched by variant on non-macOS test builds; the payload itself is
    // read by the real macOS-only consumer, `check_hosts_pin`.
    Conflicting(#[allow(dead_code)] String),
    Missing,
}

/// Send an A-record query for `name` to `server` and return the answered IPv4
/// addresses. Used to probe the embedded DNS server directly (check 4).
async fn probe_embedded_dns(server: SocketAddr, name: &str) -> Result<Vec<Ipv4Addr>> {
    let id: u16 = rand::random();
    let query = build_dns_query_a(name, id);

    let bind: SocketAddr = if server.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let sock = UdpSocket::bind(bind).await?;
    sock.connect(server).await?;
    sock.send(&query).await?;

    let mut buf = vec![0u8; 512];
    let n = match timeout(DNS_PROBE_TIMEOUT, sock.recv(&mut buf)).await {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => anyhow::bail!("timed out after {}s", DNS_PROBE_TIMEOUT.as_secs()),
    };
    buf.truncate(n);
    Ok(parse_dns_a_answers(&buf))
}

/// Build a minimal DNS query packet for an A record of `name` (recursion-desired).
fn build_dns_query_a(name: &str, id: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(32);
    pkt.extend_from_slice(&id.to_be_bytes());
    pkt.extend_from_slice(&0x0100u16.to_be_bytes()); // flags: standard query, RD
    pkt.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    pkt.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    pkt.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    pkt.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    for label in name.trim_end_matches('.').split('.') {
        let bytes = label.as_bytes();
        pkt.push(bytes.len() as u8);
        pkt.extend_from_slice(bytes);
    }
    pkt.push(0); // root label terminator
    pkt.extend_from_slice(&1u16.to_be_bytes()); // QTYPE = A
    pkt.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
    pkt
}

/// Extract the IPv4 addresses from the answer section of a DNS response.
/// Tolerant of name-compression pointers; ignores non-A records.
fn parse_dns_a_answers(resp: &[u8]) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    if resp.len() < 12 {
        return out;
    }
    let qd = u16::from_be_bytes([resp[4], resp[5]]) as usize;
    let an = u16::from_be_bytes([resp[6], resp[7]]) as usize;

    let mut pos = 12;
    // Skip the question section.
    for _ in 0..qd {
        pos = match skip_name(resp, pos) {
            Some(p) => p,
            None => return out,
        };
        pos += 4; // QTYPE + QCLASS
        if pos > resp.len() {
            return out;
        }
    }

    // Parse each answer RR.
    for _ in 0..an {
        pos = match skip_name(resp, pos) {
            Some(p) => p,
            None => return out,
        };
        if pos + 10 > resp.len() {
            return out;
        }
        let rtype = u16::from_be_bytes([resp[pos], resp[pos + 1]]);
        let rdlength = u16::from_be_bytes([resp[pos + 8], resp[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlength > resp.len() {
            return out;
        }
        if rtype == 1 && rdlength == 4 {
            out.push(Ipv4Addr::new(
                resp[pos],
                resp[pos + 1],
                resp[pos + 2],
                resp[pos + 3],
            ));
        }
        pos += rdlength;
    }
    out
}

/// Advance past a DNS name at `pos`, returning the offset just after it.
/// Handles a compression pointer (which ends the name in the current record).
fn skip_name(buf: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *buf.get(pos)?;
        if len & 0xC0 == 0xC0 {
            // Compression pointer: two bytes, and the name ends here.
            return Some(pos + 2);
        }
        if len == 0 {
            return Some(pos + 1);
        }
        pos += 1 + len as usize;
        if pos > buf.len() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_vip_range() {
        assert!(is_overlay_vip(Ipv4Addr::new(10, 254, 0, 2)));
        assert!(is_overlay_vip(Ipv4Addr::new(10, 254, 55, 9)));
        assert!(!is_overlay_vip(Ipv4Addr::new(10, 253, 0, 2)));
        assert!(!is_overlay_vip(Ipv4Addr::new(127, 0, 0, 1)));
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(format_duration(Duration::from_secs(5)), "5s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m");
        assert_eq!(format_duration(Duration::from_secs(3700)), "1h1m");
        assert_eq!(format_duration(Duration::from_secs(90_000)), "1d1h");
    }

    #[test]
    fn dns_query_is_well_formed() {
        let q = build_dns_query_a("portzero.local", 0xABCD);
        assert_eq!(&q[0..2], &[0xAB, 0xCD], "id");
        assert_eq!(&q[2..4], &[0x01, 0x00], "flags: RD");
        assert_eq!(&q[4..6], &[0x00, 0x01], "QDCOUNT = 1");
        // Question labels: 8 'portzero' 5 'local' 0, then QTYPE/QCLASS.
        assert_eq!(q[12], 8);
        assert_eq!(&q[13..21], b"portzero");
        assert_eq!(q[21], 5);
        assert_eq!(&q[22..27], b"local");
        assert_eq!(q[27], 0, "root terminator");
        assert_eq!(&q[28..30], &[0x00, 0x01], "QTYPE = A");
        assert_eq!(&q[30..32], &[0x00, 0x01], "QCLASS = IN");
    }

    #[test]
    fn parse_answer_with_compression_pointer() {
        // Response: id, flags, qd=1, an=1, ns=0, ar=0.
        let mut resp = vec![
            0xAB, 0xCD, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        ];
        // Question: portzero.local A IN.
        resp.push(8);
        resp.extend_from_slice(b"portzero");
        resp.push(5);
        resp.extend_from_slice(b"local");
        resp.push(0);
        resp.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        // Answer: name = pointer to 0x0c, A, IN, ttl=0, rdlength=4, 10.254.0.2.
        resp.extend_from_slice(&[0xC0, 0x0C]);
        resp.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        resp.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        resp.extend_from_slice(&[0x00, 0x04]);
        resp.extend_from_slice(&[10, 254, 0, 2]);

        let ips = parse_dns_a_answers(&resp);
        assert_eq!(ips, vec![Ipv4Addr::new(10, 254, 0, 2)]);
    }

    #[test]
    fn parse_answer_ignores_non_a_records() {
        // qd=1, an=1 with a CNAME (type 5) answer -> no A addresses.
        let mut resp = vec![
            0xAB, 0xCD, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        ];
        resp.push(3);
        resp.extend_from_slice(b"foo");
        resp.push(0);
        resp.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        resp.extend_from_slice(&[0xC0, 0x0C]);
        resp.extend_from_slice(&[0x00, 0x05, 0x00, 0x01]); // CNAME
        resp.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        resp.extend_from_slice(&[0x00, 0x02]);
        resp.extend_from_slice(&[0xC0, 0x0C]);

        assert!(parse_dns_a_answers(&resp).is_empty());
    }

    #[test]
    fn parse_truncated_response_is_safe() {
        assert!(parse_dns_a_answers(&[0x00, 0x01]).is_empty());
        assert!(parse_dns_a_answers(&[]).is_empty());
    }

    #[test]
    fn hosts_pin_classification() {
        assert!(matches!(
            analyze_hosts_pin("127.0.0.1 localhost\n10.254.0.2 portzero.local # pin\n"),
            HostsPin::Expected
        ));
        assert!(matches!(
            analyze_hosts_pin("127.0.0.1 portzero.local\n"),
            HostsPin::Conflicting(_)
        ));
        assert!(matches!(
            analyze_hosts_pin("127.0.0.1 localhost\n"),
            HostsPin::Missing
        ));
    }
}
