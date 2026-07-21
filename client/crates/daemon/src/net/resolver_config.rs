//! Scoped OS DNS resolver configuration for `*.portzero.local`.
//!
//! Points the OS at our embedded DNS server for ONLY the `portzero.local` domain,
//! without hijacking the whole system resolver. Three platforms are supported:
//!
//! - **macOS**: writes `/etc/resolver/portzero.local` (requires privileges).
//! - **Linux**: attaches scoped DNS + `~portzero.local` routing domain to the
//!   overlay's own TUN link via systemd-resolved's `resolve1` D-Bus API
//!   (`busctl`), with a dnsmasq-snippet fallback when resolved is absent. See
//!   the Linux section for the per-environment strategy.
//! - **Windows**: calls `Add-DnsClientNrptRule` via PowerShell.
//!
//! On unsupported platforms (other Unix platforms) the calls compile to no-ops with a
//! logged warning.
//!
//! # Design for testability
//!
//! Privilege-requiring writes are done by [`install`] / [`uninstall`].  All the
//! pure content-generation helpers (`macos_resolver_file_content`,
//! `resolvectl_setup_args`, …) are `pub(crate)` so they can be unit-tested
//! without touching the real system.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tracing::info;
#[cfg(any(
    target_os = "linux",
    not(any(target_os = "macos", target_os = "linux", target_os = "windows"))
))]
use tracing::warn;

// Public entry points

/// Install the scoped resolver for `*.portzero.local`, routing queries to
/// `dns_addr` (our embedded DNS server).
///
/// `link_name` is the name of the overlay's own TUN interface (e.g. `deven0`),
/// which the daemon created before calling this. On Linux the scoped DNS is
/// attached to that real link (not `lo`), so systemd-resolved manages it via
/// its `resolve1` D-Bus API independently of systemd-networkd. Other platforms
/// ignore `link_name`.
///
/// May require elevated privileges on macOS (needs write access to
/// `/etc/resolver/`). On Linux, the daemon already runs privileged to create
/// the TUN; talking to systemd-resolved over D-Bus does not itself require
/// root. On Windows, `Add-DnsClientNrptRule` requires Administrator.
pub async fn install(dns_addr: SocketAddr, link_name: &str) -> Result<()> {
    install_impl(dns_addr, link_name)
}

/// Remove the scoped resolver configuration installed by [`install`].
///
/// `link_name` must match the TUN interface name passed to [`install`] so the
/// per-link settings can be reverted on the correct link.
pub async fn uninstall(link_name: &str) -> Result<()> {
    uninstall_impl(link_name)
}

/// Whether the scoped `*.portzero.local` resolver configuration is currently
/// present on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverStatus {
    /// Present and — where we verify contents — well-formed.
    Present,
    /// Definitively absent: never installed, or removed out-of-band after
    /// install (e.g. a user or another tool deleted the file).
    Missing,
    /// Presence can't be determined cheaply on this host (systemd-resolved
    /// per-link D-Bus state, or a Windows NRPT rule). Callers must not treat
    /// this as "removed" — it never triggers a warn-and-repair.
    Unknown,
}

/// Cheaply check whether the scoped resolver for `*.portzero.local` is still in
/// place, so the daemon can detect out-of-band removal and repair it.
///
/// `dns_addr` is the address the resolver should point at; where the config is a
/// file we fully own (macOS, dnsmasq), a present-but-stale file counts as
/// [`ResolverStatus::Missing`] so the repair rewrites it.
pub fn status(dns_addr: SocketAddr) -> ResolverStatus {
    status_impl(dns_addr)
}

// macOS

#[cfg(target_os = "macos")]
fn install_impl(dns_addr: SocketAddr, _link_name: &str) -> Result<()> {
    use std::fs;
    use std::path::Path;

    let dir = Path::new("/etc/resolver");
    if !dir.exists() {
        fs::create_dir_all(dir).context("creating /etc/resolver")?;
    }
    let path = dir.join("portzero.local");
    let content = macos_resolver_file_content(dns_addr);
    fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    info!("macOS: wrote scoped resolver {}", path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_impl(_link_name: &str) -> Result<()> {
    use std::path::Path;
    let path = Path::new("/etc/resolver/portzero.local");
    if path.exists() {
        std::fs::remove_file(path).context("removing /etc/resolver/portzero.local")?;
        info!("macOS: removed scoped resolver /etc/resolver/portzero.local");
    }
    Ok(())
}

/// Generate the content of `/etc/resolver/portzero.local`.
/// The `port` line is needed when the server listens on a non-standard port.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn macos_resolver_file_content(dns_addr: SocketAddr) -> String {
    let mut out = String::from("# Managed by port-zero — do not edit by hand.\n");
    out.push_str(&format!("nameserver {}\n", dns_addr.ip()));
    if dns_addr.port() != 53 {
        out.push_str(&format!("port {}\n", dns_addr.port()));
    }
    out
}

#[cfg(target_os = "macos")]
fn status_impl(dns_addr: SocketAddr) -> ResolverStatus {
    match std::fs::read_to_string("/etc/resolver/portzero.local") {
        Ok(content) if content == macos_resolver_file_content(dns_addr) => ResolverStatus::Present,
        // File is gone, or present but no longer matches what we write (stale or
        // hand-edited): treat as Missing so the caller rewrites it.
        _ => ResolverStatus::Missing,
    }
}

// Linux
//
// The previous implementation attached the scoped DNS to the loopback link
// (`lo`) via `resolvectl dns lo …`. On boxes where systemd-resolved is active
// but systemd-**networkd** is NOT (the common NetworkManager setup), that path
// fails with `Unit dbus-org.freedesktop.network1.service not found` (network1 =
// networkd): resolvectl tries to coordinate the link's config with networkd,
// which is not running, so nothing gets wired up even though resolved is fine.
//
// We now detect the resolver environment and pick a strategy, attaching the
// scoped config to the overlay's OWN TUN link (e.g. `deven0`) — a real link
// resolved manages directly — instead of `lo`:
//
//   (A) systemd-resolved + networkd        -> works (per-link config on TUN)
//   (B) systemd-resolved WITHOUT networkd  -> works: talk to resolved's
//        (NetworkManager)                     `resolve1` D-Bus API directly via
//                                             `busctl`, addressing the TUN by
//                                             ifindex. This bypasses resolvectl's
//                                             networkd coordination entirely, so
//                                             the `network1` error never occurs.
//   (C) no systemd-resolved                -> dnsmasq snippet fallback, when a
//                                             dnsmasq-based resolver is detected;
//                                             otherwise a clear, actionable warn.
//
// Everything is best-effort / non-fatal with actionable warnings, matching the
// rest of the overlay's treatment of privileged operations.
//
// `busctl` (the resolve1 path) is preferred over `resolvectl dns deven0 …`
// because resolvectl still routes per-link DNS changes through networkd on some
// systemd versions. The resolvectl-on-TUN path is kept as a structured
// alternative (`resolvectl_dns_args` / `resolvectl_domain_args`) so a human can
// switch mechanisms after on-box validation tells us which one resolved accepts.

/// The routing domain we scope to. The leading-`~`/`true` "routing domain only"
/// semantics ensure general hostname resolution is never affected.
const SCOPED_DOMAIN: &str = "portzero.local";

/// Which resolver mechanism is wired up on this host. Detected at install time;
/// the matching teardown is selected the same way at uninstall time.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxResolver {
    /// systemd-resolved is active: attach per-link config to the TUN via the
    /// resolve1 D-Bus API (`busctl`).
    SystemdResolved,
    /// systemd-resolved absent but a dnsmasq-based resolver owns resolv.conf:
    /// drop a scoped `server=/portzero.local/…` snippet.
    Dnsmasq,
    /// Nothing we can configure automatically.
    None,
}

/// Inputs describing the host resolver environment, captured so detection is a
/// pure function (and therefore unit-testable without touching the system).
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
struct ResolverEnv {
    /// `systemctl is-active systemd-resolved` succeeded.
    resolved_active: bool,
    /// `/run/systemd/resolve/` exists (resolved's runtime dir).
    resolved_runtime_present: bool,
    /// A NetworkManager-managed dnsmasq instance was detected.
    nm_dnsmasq: bool,
    /// A standalone dnsmasq is on PATH / active.
    plain_dnsmasq: bool,
}

/// Pure environment -> mechanism mapping. Tested directly.
#[cfg(target_os = "linux")]
fn classify_resolver(env: &ResolverEnv) -> LinuxResolver {
    if env.resolved_active || env.resolved_runtime_present {
        LinuxResolver::SystemdResolved
    } else if env.nm_dnsmasq || env.plain_dnsmasq {
        LinuxResolver::Dnsmasq
    } else {
        LinuxResolver::None
    }
}

#[cfg(target_os = "linux")]
fn install_impl(dns_addr: SocketAddr, link_name: &str) -> Result<()> {
    let env = detect_resolver_env();
    match classify_resolver(&env) {
        LinuxResolver::SystemdResolved => {
            let ifindex = read_ifindex(link_name)
                .with_context(|| format!("reading ifindex for TUN link {link_name}"))?;
            // resolve1 SetLinkDNS + SetLinkDomains, addressed by ifindex. This
            // talks to systemd-resolved directly and does NOT involve networkd.
            run_busctl(&busctl_set_link_dns_args(ifindex, dns_addr))
                .context("busctl SetLinkDNS")?;
            run_busctl(&busctl_set_link_domains_args(ifindex)).context("busctl SetLinkDomains")?;
            info!(
                "Linux: configured systemd-resolved for {} on link {} (ifindex {}) via resolve1 D-Bus",
                SCOPED_DOMAIN, link_name, ifindex
            );
            Ok(())
        }
        LinuxResolver::Dnsmasq => {
            let path = dnsmasq_snippet_path(env.nm_dnsmasq);
            let content = dnsmasq_snippet_content(dns_addr);
            std::fs::write(&path, content)
                .with_context(|| format!("writing dnsmasq snippet {}", path.display()))?;
            reload_dnsmasq(env.nm_dnsmasq);
            info!(
                "Linux: wrote scoped dnsmasq snippet {} for {}",
                path.display(),
                SCOPED_DOMAIN
            );
            Ok(())
        }
        LinuxResolver::None => {
            warn!(
                "Linux: no supported local resolver detected (systemd-resolved inactive, \
                 no dnsmasq). *.{} will not resolve automatically. To fix, either enable \
                 systemd-resolved, or point your resolver's {} domain at {}.",
                SCOPED_DOMAIN, SCOPED_DOMAIN, dns_addr
            );
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn uninstall_impl(link_name: &str) -> Result<()> {
    let env = detect_resolver_env();
    match classify_resolver(&env) {
        LinuxResolver::SystemdResolved => {
            // Revert the per-link config we set. RevertLink restores defaults for
            // exactly this link, so no global resolver state is disturbed.
            let ifindex = read_ifindex(link_name)
                .with_context(|| format!("reading ifindex for TUN link {link_name}"))?;
            run_busctl(&busctl_revert_link_args(ifindex)).context("busctl RevertLink")?;
            info!(
                "Linux: reverted systemd-resolved settings for link {} (ifindex {})",
                link_name, ifindex
            );
            Ok(())
        }
        LinuxResolver::Dnsmasq => {
            // Remove either possible snippet location, best-effort.
            for nm in [true, false] {
                let path = dnsmasq_snippet_path(nm);
                if path.exists() {
                    if let Err(e) = std::fs::remove_file(&path) {
                        warn!(
                            "Linux: failed to remove dnsmasq snippet {}: {e}",
                            path.display()
                        );
                    } else {
                        info!("Linux: removed dnsmasq snippet {}", path.display());
                        reload_dnsmasq(nm);
                    }
                }
            }
            Ok(())
        }
        LinuxResolver::None => Ok(()),
    }
}

#[cfg(target_os = "linux")]
fn status_impl(_dns_addr: SocketAddr) -> ResolverStatus {
    let env = detect_resolver_env();
    match classify_resolver(&env) {
        // systemd-resolved config is per-link D-Bus state, not a file we can
        // cheaply stat; don't attempt removal-detection for it.
        LinuxResolver::SystemdResolved | LinuxResolver::None => ResolverStatus::Unknown,
        LinuxResolver::Dnsmasq => {
            if dnsmasq_snippet_path(env.nm_dnsmasq).exists() {
                ResolverStatus::Present
            } else {
                ResolverStatus::Missing
            }
        }
    }
}

// resolve1 (busctl) argument builders — pure, unit-tested

/// Build `busctl call … SetLinkDNS` args for an IPv4/IPv6 `dns_addr` on `ifindex`.
///
/// resolve1 `SetLinkDNS` signature: `ia(iay)` = link ifindex, then an array of
/// (address-family, address-bytes) pairs. We pass exactly one server. Note that
/// `SetLinkDNS` carries no port, so the embedded DNS must listen on port 53 for
/// this path; the dnsmasq fallback and macOS/Windows paths do carry the port.
/// (`SetLinkDNSEx` adds port+SNI but is less widely available; kept simple here.)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn busctl_set_link_dns_args(ifindex: u32, dns_addr: SocketAddr) -> Vec<String> {
    let ip = dns_addr.ip();
    let (family, bytes): (i32, Vec<u8>) = match ip {
        std::net::IpAddr::V4(v4) => (libc_af_inet(), v4.octets().to_vec()),
        std::net::IpAddr::V6(v6) => (libc_af_inet6(), v6.octets().to_vec()),
    };
    let mut args = vec![
        "call".to_string(),
        "org.freedesktop.resolve1".to_string(),
        "/org/freedesktop/resolve1".to_string(),
        "org.freedesktop.resolve1.Manager".to_string(),
        "SetLinkDNS".to_string(),
        "ia(iay)".to_string(),
        ifindex.to_string(),
        "1".to_string(), // one DNS server in the array
        family.to_string(),
        bytes.len().to_string(),
    ];
    for b in bytes {
        args.push(b.to_string());
    }
    args
}

/// Build `busctl call … SetLinkDomains` args restricting `ifindex` to the
/// `portzero.local` ROUTING domain.
///
/// resolve1 `SetLinkDomains` signature: `ia(sb)` = link ifindex, then an array
/// of (domain, routing-only?) pairs. `routing-only = true` is the D-Bus
/// equivalent of resolvectl's `~portzero.local`: queries for this domain are
/// routed to this link's DNS but it is never used as a search domain.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn busctl_set_link_domains_args(ifindex: u32) -> Vec<String> {
    vec![
        "call".to_string(),
        "org.freedesktop.resolve1".to_string(),
        "/org/freedesktop/resolve1".to_string(),
        "org.freedesktop.resolve1.Manager".to_string(),
        "SetLinkDomains".to_string(),
        "ia(sb)".to_string(),
        ifindex.to_string(),
        "1".to_string(), // one domain in the array
        SCOPED_DOMAIN.to_string(),
        "true".to_string(), // routing-domain only (not a search domain)
    ]
}

/// Build `busctl call … RevertLink` args to restore defaults for `ifindex`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn busctl_revert_link_args(ifindex: u32) -> Vec<String> {
    vec![
        "call".to_string(),
        "org.freedesktop.resolve1".to_string(),
        "/org/freedesktop/resolve1".to_string(),
        "org.freedesktop.resolve1.Manager".to_string(),
        "RevertLink".to_string(),
        "i".to_string(),
        ifindex.to_string(),
    ]
}

// resolvectl-on-TUN alternative (kept for easy mechanism swap)

/// Returns the resolvectl args to set the DNS server for a link.
/// Exported for testing. Alternative to the busctl/resolve1 path: pass the TUN
/// link name (e.g. `deven0`), NOT `lo`. Kept (not wired in by default) so the
/// mechanism can be swapped after on-box validation; hence `allow(dead_code)`.
#[allow(dead_code)]
pub(crate) fn resolvectl_dns_args(link: &str, dns_addr: SocketAddr) -> Vec<String> {
    // resolvectl dns <link> <ip>:<port>  (port supported since systemd 245)
    let addr_str = format!("{}:{}", dns_addr.ip(), dns_addr.port());
    vec!["dns".to_string(), link.to_string(), addr_str]
}

/// Returns the resolvectl args to restrict a link to the portzero.local domain.
/// Kept as the alternative mechanism to the busctl/resolve1 path; see
/// [`resolvectl_dns_args`].
#[allow(dead_code)]
pub(crate) fn resolvectl_domain_args(link: &str) -> Vec<String> {
    // ~portzero.local means "routing domain only" — queries for portzero.local go
    // here but it is not used as a search domain.
    vec![
        "domain".to_string(),
        link.to_string(),
        format!("~{SCOPED_DOMAIN}"),
    ]
}

// dnsmasq fallback snippet — pure builders, unit-tested

/// Path of the scoped dnsmasq snippet. NetworkManager's integrated dnsmasq
/// reads `/etc/NetworkManager/dnsmasq.d/`; a standalone dnsmasq reads
/// `/etc/dnsmasq.d/`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn dnsmasq_snippet_path(nm: bool) -> std::path::PathBuf {
    if nm {
        std::path::PathBuf::from("/etc/NetworkManager/dnsmasq.d/portzero.conf")
    } else {
        std::path::PathBuf::from("/etc/dnsmasq.d/portzero.conf")
    }
}

/// Scoped dnsmasq snippet: route only `portzero.local` to our embedded DNS,
/// preserving the port. `server=/portzero.local/<ip>#<port>`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn dnsmasq_snippet_content(dns_addr: SocketAddr) -> String {
    format!(
        "# Managed by port-zero — do not edit by hand.\n\
         server=/{domain}/{ip}#{port}\n",
        domain = SCOPED_DOMAIN,
        ip = dns_addr.ip(),
        port = dns_addr.port(),
    )
}

// ifindex parsing — pure, unit-tested

/// Parse the contents of `/sys/class/net/<link>/ifindex` into a link index.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_ifindex(contents: &str) -> Result<u32> {
    contents
        .trim()
        .parse::<u32>()
        .with_context(|| format!("unexpected ifindex contents: {contents:?}"))
}

#[cfg(target_os = "linux")]
fn read_ifindex(link_name: &str) -> Result<u32> {
    let path = format!("/sys/class/net/{link_name}/ifindex");
    let contents = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    parse_ifindex(&contents)
}

// system detection (impure shims kept thin)

#[cfg(target_os = "linux")]
fn detect_resolver_env() -> ResolverEnv {
    ResolverEnv {
        resolved_active: systemctl_is_active("systemd-resolved"),
        resolved_runtime_present: std::path::Path::new("/run/systemd/resolve").exists(),
        nm_dnsmasq: std::path::Path::new("/etc/NetworkManager/dnsmasq.d").exists()
            && command_on_path("dnsmasq"),
        plain_dnsmasq: command_on_path("dnsmasq")
            && std::path::Path::new("/etc/dnsmasq.d").exists(),
    }
}

#[cfg(target_os = "linux")]
fn systemctl_is_active(unit: &str) -> bool {
    matches!(
        std::process::Command::new("systemctl")
            .args(["is-active", "--quiet", unit])
            .status(),
        Ok(s) if s.success()
    )
}

#[cfg(target_os = "linux")]
fn command_on_path(cmd: &str) -> bool {
    matches!(
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1")])
            .status(),
        Ok(s) if s.success()
    )
}

#[cfg(target_os = "linux")]
fn reload_dnsmasq(nm: bool) {
    // Best-effort reload so the new snippet takes effect. NetworkManager owns
    // its embedded dnsmasq; a standalone dnsmasq is a systemd unit.
    let (program, args): (&str, &[&str]) = if nm {
        ("systemctl", &["reload-or-restart", "NetworkManager"])
    } else {
        ("systemctl", &["reload-or-restart", "dnsmasq"])
    };
    if let Err(e) = std::process::Command::new(program).args(args).status() {
        warn!(
            "Linux: failed to reload resolver ({program} {}): {e}",
            args.join(" ")
        );
    }
}

#[cfg(target_os = "linux")]
fn run_busctl(args: &[impl AsRef<std::ffi::OsStr>]) -> Result<()> {
    let status = std::process::Command::new("busctl")
        .args(args)
        .status()
        .context("running busctl (is systemd-resolved active?)")?;
    if !status.success() {
        anyhow::bail!("busctl exited with {}", status);
    }
    Ok(())
}

/// `AF_INET` / `AF_INET6` constants used in the resolve1 `(iay)` address family
/// field. Defined here (rather than pulling in libc just for two ints) so the
/// pure arg builders need no platform crate; they are the stable Linux values.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const fn libc_af_inet() -> i32 {
    2
}
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const fn libc_af_inet6() -> i32 {
    10
}

// Windows

#[cfg(target_os = "windows")]
fn install_impl(dns_addr: SocketAddr, _link_name: &str) -> Result<()> {
    let script = powershell_add_nrpt_script(dns_addr);
    run_powershell(&script).context("Add-DnsClientNrptRule")?;
    info!(
        "Windows: added NRPT rule for portzero.local -> {}",
        dns_addr
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn uninstall_impl(_link_name: &str) -> Result<()> {
    let script = powershell_remove_nrpt_script();
    run_powershell(&script).context("Remove-DnsClientNrptRule")?;
    info!("Windows: removed NRPT rule for portzero.local");
    Ok(())
}

/// Synchronously remove the `.portzero.local` NRPT rule.
///
/// `portzero stop` terminates the Windows daemon with `taskkill /F` (there is
/// no graceful console signal to deliver to a detached process), so the
/// daemon's own overlay teardown — which removes this rule — never runs.
/// `stop` calls this after the kill so no DNS residue outlives the daemon;
/// must run AFTER the daemon is gone, or its resolver-repair loop would
/// recreate the rule.
#[cfg(target_os = "windows")]
pub fn remove_nrpt_rule_sync() -> Result<()> {
    uninstall_impl("")
}

/// PowerShell snippet that adds (or replaces) the NRPT rule.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn powershell_add_nrpt_script(dns_addr: SocketAddr) -> String {
    // Remove any existing rule for this namespace first to make it idempotent.
    // NRPT requires the namespace with a leading dot.
    format!(
        r#"
Get-DnsClientNrptRule | Where-Object {{ $_.Namespace -eq '.portzero.local' }} | Remove-DnsClientNrptRule -Force -ErrorAction SilentlyContinue
Add-DnsClientNrptRule -Namespace '.portzero.local' -NameServers '{ip}' -Comment 'port-zero managed'
"#,
        ip = dns_addr.ip()
    )
}

/// PowerShell snippet that removes the NRPT rule.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn powershell_remove_nrpt_script() -> String {
    r#"
Get-DnsClientNrptRule | Where-Object { $_.Namespace -eq '.portzero.local' } | Remove-DnsClientNrptRule -Force -ErrorAction SilentlyContinue
"#
    .to_string()
}

#[cfg(target_os = "windows")]
fn status_impl(_dns_addr: SocketAddr) -> ResolverStatus {
    // Checking an NRPT rule requires a PowerShell query per call; skip
    // removal-detection to keep the periodic check cheap.
    ResolverStatus::Unknown
}

#[cfg(target_os = "windows")]
fn run_powershell(script: &str) -> Result<()> {
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .status()
        .context("running powershell.exe")?;
    if !status.success() {
        anyhow::bail!("PowerShell exited with {}", status);
    }
    Ok(())
}

// Unsupported platforms — compile to no-ops with a warning

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn install_impl(dns_addr: SocketAddr, _link_name: &str) -> Result<()> {
    warn!(
        "scoped resolver not supported on this platform; \
         configure your OS to send *.portzero.local queries to {}",
        dns_addr
    );
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn uninstall_impl(_link_name: &str) -> Result<()> {
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn status_impl(_dns_addr: SocketAddr) -> ResolverStatus {
    ResolverStatus::Unknown
}

// Unit tests — purely functional; never write to the real system

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn addr(ip: &str, port: u16) -> SocketAddr {
        SocketAddr::new(ip.parse::<IpAddr>().unwrap(), port)
    }

    // --- macOS ---

    #[test]
    fn macos_content_standard_port() {
        let content = macos_resolver_file_content(addr("127.0.0.1", 53));
        assert!(
            content.contains("nameserver 127.0.0.1\n"),
            "missing nameserver line"
        );
        assert!(
            !content.contains("\nport "),
            "should not emit port line for port 53"
        );
    }

    #[test]
    fn macos_content_custom_port() {
        let content = macos_resolver_file_content(addr("127.0.0.1", 5300));
        assert!(content.contains("nameserver 127.0.0.1\n"));
        assert!(
            content.contains("port 5300\n"),
            "should emit port line for non-53 port"
        );
    }

    #[test]
    fn macos_content_comment() {
        let content = macos_resolver_file_content(addr("127.0.0.1", 5300));
        assert!(
            content.contains("port-zero"),
            "should have management comment"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_status_missing_when_file_absent() {
        // The real file path won't match our test content unless port-zero is
        // installed; either way, a mismatched/absent file must read as Missing,
        // never as Present with foreign content. (Present is only asserted in the
        // integration path where install() wrote the exact bytes.)
        let s = status(addr("127.0.0.1", 1));
        assert_ne!(
            s,
            ResolverStatus::Unknown,
            "macOS must give a definitive Present/Missing answer"
        );
    }

    // --- Linux ---

    #[test]
    fn resolvectl_dns_args_format() {
        let args = resolvectl_dns_args("lo", addr("127.0.0.1", 5300));
        assert_eq!(args[0], "dns");
        assert_eq!(args[1], "lo");
        assert_eq!(args[2], "127.0.0.1:5300");
    }

    #[test]
    fn resolvectl_domain_args_format() {
        let args = resolvectl_domain_args("lo");
        assert_eq!(args[0], "domain");
        assert_eq!(args[1], "lo");
        assert_eq!(
            args[2], "~portzero.local",
            "must use routing-domain tilde prefix"
        );
    }

    #[test]
    fn resolvectl_domain_not_search_domain() {
        // The domain must start with '~' so it is a routing domain, not a
        // search domain that would affect general hostname resolution.
        let args = resolvectl_domain_args("eth0");
        assert!(
            args[2].starts_with('~'),
            "domain must start with '~' to be a routing domain"
        );
    }

    // --- Windows ---

    #[test]
    fn powershell_add_script_contains_namespace() {
        let script = powershell_add_nrpt_script(addr("127.0.0.1", 5300));
        assert!(
            script.contains(".portzero.local"),
            "must reference .portzero.local namespace"
        );
        assert!(
            script.contains("127.0.0.1"),
            "must reference the DNS server IP"
        );
        assert!(
            script.contains("Add-DnsClientNrptRule"),
            "must call Add-DnsClientNrptRule"
        );
    }

    #[test]
    fn powershell_add_script_idempotent() {
        // The script must remove any existing rule before adding, for idempotency.
        let script = powershell_add_nrpt_script(addr("127.0.0.1", 5300));
        assert!(
            script.contains("Remove-DnsClientNrptRule"),
            "must clean up existing rule before adding"
        );
    }

    #[test]
    fn powershell_remove_script_targets_namespace() {
        let script = powershell_remove_nrpt_script();
        assert!(script.contains(".portzero.local"));
        assert!(script.contains("Remove-DnsClientNrptRule"));
    }

    // --- Scope guard: only portzero.local ---

    #[test]
    fn macos_does_not_mention_other_domains() {
        let content = macos_resolver_file_content(addr("127.0.0.1", 5300));
        // The resolver file contains no wildcard that would affect other domains.
        assert!(
            !content.contains("search"),
            "resolver file must not add search domains"
        );
    }

    #[test]
    fn resolvectl_args_scope_only_portzero_local() {
        let domain_args = resolvectl_domain_args("lo");
        // Only one domain listed, and it must be ~portzero.local.
        assert_eq!(
            domain_args.len(),
            3,
            "exactly [domain, link, ~portzero.local]"
        );
        assert_eq!(domain_args[2], "~portzero.local");
    }

    // --- Linux: resolvectl on the TUN link (not lo) ---

    #[cfg(target_os = "linux")]
    #[test]
    fn resolvectl_dns_args_use_tun_link() {
        // The fix attaches to the overlay's own TUN link, never `lo`.
        let args = resolvectl_dns_args("deven0", addr("127.0.0.1", 5300));
        assert_eq!(args, vec!["dns", "deven0", "127.0.0.1:5300"]);
        assert_ne!(
            args[1], "lo",
            "must not attach scoped DNS to the loopback link"
        );
    }

    // --- Linux: resolve1 / busctl argument builders ---

    #[cfg(target_os = "linux")]
    #[test]
    fn busctl_set_link_dns_ipv4() {
        let args = busctl_set_link_dns_args(7, addr("127.0.0.1", 5300));
        // Prefix: call <dest> <path> <iface> SetLinkDNS ia(iay) <ifindex> <count> <family> <len> <bytes...>
        assert_eq!(args[0], "call");
        assert_eq!(args[1], "org.freedesktop.resolve1");
        assert_eq!(args[2], "/org/freedesktop/resolve1");
        assert_eq!(args[3], "org.freedesktop.resolve1.Manager");
        assert_eq!(args[4], "SetLinkDNS");
        assert_eq!(args[5], "ia(iay)");
        assert_eq!(args[6], "7", "ifindex");
        assert_eq!(args[7], "1", "exactly one DNS server");
        assert_eq!(args[8], "2", "AF_INET");
        assert_eq!(args[9], "4", "IPv4 is 4 bytes");
        assert_eq!(&args[10..], &["127", "0", "0", "1"], "address bytes");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn busctl_set_link_dns_ipv6_family() {
        let args = busctl_set_link_dns_args(3, addr("::1", 5300));
        assert_eq!(args[5], "ia(iay)");
        assert_eq!(args[8], "10", "AF_INET6");
        assert_eq!(args[9], "16", "IPv6 is 16 bytes");
        // ::1 -> 15 zero bytes then 1.
        assert_eq!(args.last().map(String::as_str), Some("1"));
        assert_eq!(args.len(), 10 + 16, "16 address bytes follow the length");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn busctl_set_link_domains_routing_only() {
        let args = busctl_set_link_domains_args(7);
        assert_eq!(args[4], "SetLinkDomains");
        assert_eq!(args[5], "ia(sb)");
        assert_eq!(args[6], "7", "ifindex");
        assert_eq!(args[7], "1", "exactly one domain");
        assert_eq!(args[8], "portzero.local");
        assert_eq!(
            args[9], "true",
            "routing-domain only — must not be used as a search domain"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn busctl_revert_link_targets_ifindex() {
        let args = busctl_revert_link_args(42);
        assert_eq!(args[4], "RevertLink");
        assert_eq!(args[5], "i");
        assert_eq!(args[6], "42");
    }

    // --- Linux: ifindex parsing ---

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_ifindex_trims_and_parses() {
        assert_eq!(parse_ifindex("7\n").unwrap(), 7);
        assert_eq!(parse_ifindex("  12 ").unwrap(), 12);
        assert!(parse_ifindex("not-a-number").is_err());
        assert!(parse_ifindex("").is_err());
    }

    // --- Linux: resolver environment classification ---

    #[cfg(target_os = "linux")]
    fn env(
        resolved_active: bool,
        resolved_runtime_present: bool,
        nm_dnsmasq: bool,
        plain_dnsmasq: bool,
    ) -> ResolverEnv {
        ResolverEnv {
            resolved_active,
            resolved_runtime_present,
            nm_dnsmasq,
            plain_dnsmasq,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_prefers_systemd_resolved() {
        // (A) resolved + (implicitly) networkd, and (B) resolved without
        // networkd both classify as SystemdResolved — the mechanism (per-link
        // busctl on the TUN) is networkd-independent and handles both.
        assert_eq!(
            classify_resolver(&env(true, false, false, false)),
            LinuxResolver::SystemdResolved
        );
        assert_eq!(
            classify_resolver(&env(false, true, false, false)),
            LinuxResolver::SystemdResolved
        );
        // resolved wins even if dnsmasq is also present.
        assert_eq!(
            classify_resolver(&env(true, true, true, true)),
            LinuxResolver::SystemdResolved
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_falls_back_to_dnsmasq() {
        assert_eq!(
            classify_resolver(&env(false, false, true, false)),
            LinuxResolver::Dnsmasq
        );
        assert_eq!(
            classify_resolver(&env(false, false, false, true)),
            LinuxResolver::Dnsmasq
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn classify_none_when_nothing_detected() {
        assert_eq!(
            classify_resolver(&env(false, false, false, false)),
            LinuxResolver::None
        );
    }

    // --- Linux: dnsmasq snippet ---

    #[cfg(target_os = "linux")]
    #[test]
    fn dnsmasq_snippet_scopes_only_portzero_local() {
        let content = dnsmasq_snippet_content(addr("127.0.0.1", 5300));
        assert!(
            content.contains("server=/portzero.local/127.0.0.1#5300"),
            "must route only portzero.local to the embedded DNS with its port: {content}"
        );
        // No catch-all server line that would hijack the whole resolver.
        assert!(
            !content.lines().any(|l| l.trim() == "server=127.0.0.1#5300"),
            "must not add an unscoped server line"
        );
        assert!(content.contains("port-zero"), "management comment");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dnsmasq_snippet_path_selection() {
        assert_eq!(
            dnsmasq_snippet_path(true),
            std::path::PathBuf::from("/etc/NetworkManager/dnsmasq.d/portzero.conf")
        );
        assert_eq!(
            dnsmasq_snippet_path(false),
            std::path::PathBuf::from("/etc/dnsmasq.d/portzero.conf")
        );
    }
}
