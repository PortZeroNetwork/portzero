//! Service discovery: scan processes and Docker containers for PZ_TUNNEL.
//!
//! The value of PZ_TUNNEL must be a full domain name (after template
//! substitution). No implicit suffixes are added.
//!
//! - If it ends with `.portzero.local` (or `.local`) → local virtual overlay.
//! - Otherwise it must be a valid tunnel domain ending in `.tunnel.portzero.cloud`
//!   (or the configured base), under a username scope like `api.alice.tunnel.portzero.cloud`.
//!
//! Examples (full names required):
//!   PZ_TUNNEL=my-api.alice.tunnel.portzero.cloud
//!   PZ_TUNNEL=my-db-{branch}.portzero.local
//!   PZ_TUNNEL=web-{branch}--{cloud-username}.tunnel.portzero.cloud
//!
//! `{local-username}` (OS username, always available) and `{cloud-username}`
//! (cloud account username, requires login) are distinct placeholders —
//! `{local-username}` may not appear in a cloud tunnel template, and
//! `{cloud-username}` may appear in a `.local` template but needs login to
//! resolve. See `portzero_domain::validate_username_placeholders`.
//!
//! Cross-platform support:
//! - Linux: full (reads /proc/<pid>/environ and /proc/<pid>/fd for inode-based port scoping)
//! - macOS: full (uses `ps -p <pid> -wwwE` and `lsof`)
//! - Windows: full for same-bitness/readable processes (reads the target PEB
//!   environment block and uses `Get-NetTCPConnection` for port ownership)

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use anyhow::Result;
use portzero_domain::{split_tunnel_port, validate_tunnel_domain, DomainContext};
use sysinfo::System;

use crate::protocol_detect;
mod docker;
mod process;
mod system_listeners;

#[allow(unused_imports)]
use docker::{parse_docker_ports, scan_docker_containers, scan_network_containers};
#[allow(unused_imports)]
use process::{
    discover_process_ports, looks_like_repo_path, parse_extra_ports, parse_http_port_selection,
    resolve_tunnel_template, scan_network_processes, scan_process_env, scan_processes,
    select_http_port, template_substitutions, warn_if_port_like_rejected,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(unused_imports)]
use process::{parse_lsof_line, parse_lsof_stdout};

#[cfg(target_os = "macos")]
#[allow(unused_imports)]
use process::{parse_macos_ps_env_candidate, parse_macos_ps_env_candidates, parse_procargs2_env};

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
use process::parse_proc_net_tcp_line;

#[cfg(target_os = "windows")]
#[allow(unused_imports)]
use process::{
    parse_windows_environment_block, parse_windows_netstat_line, parse_windows_netstat_stdout,
    parse_windows_netstat_stdout_by_pid, parse_windows_tcp_connection_line,
    parse_windows_tcp_connection_stdout,
};
pub use system_listeners::{enumerate_system_listeners, SystemListener};

/// The single environment variable used to tag services.
///
/// The value (after substitution) must be a full domain name:
/// - `*.portzero.local` → local overlay
/// - `<name>--<username>.tunnel.portzero.cloud` (or configured base) → cloud tunnel
///
/// No implicit suffix is ever appended.
const ENV_VAR_NAME: &str = "PZ_TUNNEL";

/// Selects which HTTP port to forward. Defaults to CHOOSE_LOWEST if unset.
const ENV_HTTP_PORT_VAR: &str = "PZ_TUNNEL_HTTP_PORT";

/// Additional raw port mappings: `local:tunnel[;local:tunnel...]`
const ENV_PORTS_VAR: &str = "PZ_TUNNEL_PORTS";

/// Opt-out for active protocol probing (task-17). When set on the target
/// process (or on the daemon's own env) probing is skipped entirely.
const ENV_NO_PROBE_VAR: &str = "PZ_TUNNEL_NO_PROBE";

/// Optional HTTP path that signals the tunneled endpoint is ready (e.g.
/// `/health`). Discovered alongside `PZ_TUNNEL`; entirely optional. Consumed by
/// `portzero wait --healthy` and surfaced in `portzero status` / `portzero
/// inspect`. The value survives graduation to a PaaS even though the `PZ_*` var
/// itself does not.
const ENV_HEALTH_PATH_VAR: &str = "PZ_HEALTH_PATH";

/// Normalize a raw `PZ_HEALTH_PATH` value into a rooted HTTP path.
///
/// - Trims surrounding whitespace.
/// - Returns `None` for an empty value (fully optional — absent changes nothing).
/// - Ensures a single leading `/` so `health`, `/health`, and ` health ` all
///   normalize to `/health`.
pub fn normalize_health_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('/') {
        Some(trimmed.to_string())
    } else {
        Some(format!("/{trimmed}"))
    }
}

/// Returns true if the (full) resolved name indicates the local virtual overlay
/// (must end with .portzero.local or .local).
///
/// The value in PZ_TUNNEL must be the complete name; no suffix is added.
pub fn is_local_overlay_domain(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n.ends_with(".portzero.local") || n == "portzero.local" || n.ends_with(".local")
}

/// Overlay names the daemon reserves for itself. Both `portzero.local` and
/// `api.portzero.local` resolve to the daemon's own management server (the
/// legacy browser dashboard at `/` and the `/v1/*` REST API); the two
/// `portzero*.portzero.local` forms are the same names a user might otherwise
/// try to register. Nothing user-owned may claim these, and — crucially — the
/// daemon must never treat one as a discovered customer tunnel to auto-open or
/// to render as a clickable link.
pub const RESERVED_OVERLAY_DOMAINS: &[&str] = &[
    "portzero.local",
    "api.portzero.local",
    "portzero.portzero.local",
    "portzero-api.portzero.local",
];

/// True when `domain` is one of the [`RESERVED_OVERLAY_DOMAINS`] (case-insensitive).
pub fn is_reserved_overlay_domain(domain: &str) -> bool {
    let d = domain.trim().to_ascii_lowercase();
    RESERVED_OVERLAY_DOMAINS
        .iter()
        .any(|reserved| d == *reserved)
}

/// Extract the name for the overlay from a full name like "my-db.portzero.local".
/// The input must already be a full domain (no implicit suffix added by us).
pub fn extract_local_label(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    let core = n
        .strip_suffix(".portzero.local")
        .or_else(|| n.strip_suffix(".local"))
        .unwrap_or(&n);
    sanitize_network_name(core)
}

/// A discovered service with its domain, port, and origin.
#[derive(Debug, Clone)]
pub struct DiscoveredService {
    /// Resolved full PZ_TUNNEL value (must be a complete domain name).
    pub domain: String,
    /// Original PZ_TUNNEL domain value before template substitution.
    pub domain_template: String,
    /// Values available for template substitution when this service was found.
    pub substitutions: BTreeMap<String, String>,
    /// Selected HTTP port (0 if not determinable).
    pub port: u16,
    /// Address to forward to for `port` — the loopback address of the family
    /// the process is actually bound to, so an IPv6-only listener is reached
    /// on `::1` rather than an empty `127.0.0.1`.
    pub host: IpAddr,
    /// Additional port mappings from PZ_TUNNEL_PORTS.
    pub extra_ports: Vec<PortMapping>,
    /// Optional readiness path from PZ_HEALTH_PATH (e.g. "/health"), normalized
    /// to be rooted. `None` when the endpoint does not declare one.
    pub health_path: Option<String>,
    /// Process ID that owns the service.
    pub pid: u32,
    /// How the service was discovered.
    pub source: ServiceSource,
}

/// A local-port → tunnel-port mapping for raw (non-HTTP) forwarding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortMapping {
    pub local_port: u16,
    pub tunnel_port: u16,
}

/// Where the service was discovered.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ServiceSource {
    /// A local OS process.
    Process {
        /// Working directory of the process, if available.
        cwd: Option<PathBuf>,
    },
    /// A Docker container.
    Container {
        /// Container ID (short hash).
        id: String,
        /// Container name.
        name: String,
    },
}

impl std::fmt::Display for ServiceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceSource::Process { cwd } => {
                if let Some(dir) = cwd {
                    let display = if let Some(home) = dirs::home_dir() {
                        if let Ok(rel) = dir.strip_prefix(&home) {
                            format!("~/{}", rel.display())
                        } else {
                            dir.display().to_string()
                        }
                    } else {
                        dir.display().to_string()
                    };
                    write!(f, "({})", display)
                } else {
                    write!(f, "(unknown dir)")
                }
            }
            ServiceSource::Container { name, .. } => {
                write!(f, "container {}", name)
            }
        }
    }
}

/// One TCP socket a process is listening on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListeningPort {
    port: u16,
    /// The address the socket is bound to, exactly as the OS reports it:
    /// a wildcard (`0.0.0.0` / `::`), a loopback address (`127.0.0.1` / `::1`),
    /// or a specific interface address.
    ///
    /// The address family matters as much as the address: a process on `::1`
    /// is unreachable from `127.0.0.1` even though both are loopback, which is
    /// why this is kept rather than collapsed into a public/loopback flag.
    addr: IpAddr,
}

impl ListeningPort {
    fn new(addr: IpAddr, port: u16) -> Self {
        Self { port, addr }
    }

    /// True when the socket is bound to every interface (`0.0.0.0` / `::`).
    ///
    /// Port selection prefers these: a process that exposes one port publicly
    /// and keeps others on loopback (a debugger, a metrics endpoint) is
    /// telling us which one it means to serve.
    fn is_wildcard(&self) -> bool {
        self.addr.is_unspecified()
    }

    /// The address the daemon must dial to reach this listener.
    ///
    /// A wildcard bind is reachable on loopback *of its own family*:
    /// `0.0.0.0` → `127.0.0.1`, and `::` → `::1` (a `::` socket accepts `::1`
    /// whether or not `IPV6_V6ONLY` is set, so this is the safe choice for
    /// either). Anything else is dialed exactly as it was bound — including
    /// `::1`, which the daemon used to be unable to reach at all.
    fn dial_addr(&self) -> IpAddr {
        match self.addr {
            IpAddr::V4(a) if a.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(a) if a.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            other => other,
        }
    }
}

#[cfg(test)]
impl ListeningPort {
    fn v4_any(port: u16) -> Self {
        Self::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port)
    }

    fn v4_loopback(port: u16) -> Self {
        Self::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn v6_any(port: u16) -> Self {
        Self::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port)
    }

    fn v6_loopback(port: u16) -> Self {
        Self::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
    }
}

#[derive(Debug, Clone, Default)]
struct ProcessTemplateContext {
    cwd: Option<PathBuf>,
    project_dir: Option<PathBuf>,
}

/// How the HTTP port is chosen when PZ_TUNNEL_HTTP_PORT is set.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HttpPortSelection {
    /// Use this exact port number.
    Explicit(u16),
    /// Pick the lowest-numbered public port; fall back to lowest loopback.
    ChooseLowest,
    /// Pick the highest-numbered public port; fall back to highest loopback.
    ChooseHighest,
}

/// The result of selecting an HTTP port for a process.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedPort {
    /// A port was found and the process owns it.
    Found(u16),
    /// An explicit port was requested but the process does not own it.
    ExplicitNotOwned(u16),
    /// No listening port could be found (process hasn't bound one yet, or
    /// port enumeration failed).
    NoneListening,
}

/// Scan all processes and Docker containers for PZ_TUNNEL.
///
/// Returns the discovered services alongside any [`crate::notify::Issue`]s
/// found along the way (e.g. `PZ_TUNNEL` values that look like a cloud tunnel
/// request but are missing the required username scope), so callers can
/// surface both through `portzero status`.
pub async fn scan_all(
    account_id: Option<&str>,
    username: Option<&str>,
) -> (Vec<DiscoveredService>, Vec<crate::notify::Issue>) {
    let mut services = Vec::new();
    let mut issues = Vec::new();
    // `scan_processes` does a full sysinfo refresh and shells out (`ps`/`lsof`)
    // for every process system-wide, all synchronously. Run it on a blocking
    // thread so the async task yields control — otherwise the executor thread is
    // held for the whole (potentially multi-second) scan and the daemon's
    // shutdown-signal future never gets polled mid-scan.
    let acct = account_id.map(str::to_owned);
    let user = username.map(str::to_owned);
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        tokio::task::spawn_blocking(move || scan_processes(acct.as_deref(), user.as_deref())),
    )
    .await
    {
        Ok(Ok((process_services, process_issues))) => {
            services.extend(process_services);
            issues.extend(process_issues);
        }
        Ok(Err(_)) => tracing::debug!("process scan panicked"),
        Err(_) => tracing::debug!("process scan timed out"),
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        scan_docker_containers(account_id, username),
    )
    .await
    {
        Ok((docker_services, docker_issues)) => {
            services.extend(docker_services);
            issues.extend(docker_issues);
        }
        Err(_) => tracing::debug!("Docker container scan timed out"),
    }
    (services, issues)
}

/// A service discovered for the local virtual overlay network.
#[derive(Debug, Clone)]
pub struct DiscoveredNetworkService {
    /// The local overlay name extracted from the PZ_TUNNEL value (e.g. "my-db"
    /// from "my-db.portzero.local", or "staging.portzero.net" from
    /// "staging.portzero.net.portzero.local").
    pub name: String,
    /// The actual host address we must proxy to (usually 127.0.0.1:random).
    pub real_addr: std::net::SocketAddr,
    /// The port that should be presented on the virtual side (e.g. 5432).
    /// For Docker this is the container port from the -p mapping.
    /// For plain processes we use the discovered listening port.
    pub service_port: u16,
    /// Best-effort backend protocol detection, used by the overlay to avoid
    /// raw-proxying browser TLS into plaintext HTTP services on port 443.
    pub backend_protocol: Option<protocol_detect::Canonical>,
    /// Owning PID.
    pub pid: u32,
    pub source: ServiceSource,
    /// Original PZ_TUNNEL domain value before template substitution.
    pub domain_template: String,
    /// Values available for template substitution when this service was found.
    pub substitutions: BTreeMap<String, String>,
    /// Optional readiness path from PZ_HEALTH_PATH (e.g. "/health"), normalized
    /// to be rooted. `None` when the endpoint does not declare one.
    pub health_path: Option<String>,
}

/// Scan for services that should participate in the virtual overlay.
///
/// Only `PZ_TUNNEL` values whose resolved name ends with `.portzero.local`
/// (or `.local`) are accepted. The full name must be provided (no implicit
/// suffix).
///
/// Supports templating, e.g.:
///   PZ_TUNNEL=my-db-{branch}.portzero.local
///   PZ_TUNNEL={service}-{worktree}.portzero.local
///
/// The label (left part) gets a stable virtual IP under .portzero.local.
/// The daemon should surface loud errors if the same name is claimed by
/// multiple distinct worktrees.
pub async fn scan_network_services(
    mgmt_store: &crate::management::RegistrationStore,
) -> Vec<DiscoveredNetworkService> {
    // Snapshot the registration store so we can filter out PIDs that have
    // already registered through the management API and synthesize overlay
    // entries for them instead.
    let registrations = mgmt_store.read().await.clone();
    let registered_pids: std::collections::HashSet<u32> = registrations.keys().copied().collect();

    let mut out = Vec::new();
    match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        scan_network_processes(&registered_pids),
    )
    .await
    {
        Ok(process_services) => out.extend(process_services),
        Err(_) => tracing::debug!("overlay process scan timed out"),
    }
    match tokio::time::timeout(std::time::Duration::from_secs(2), scan_network_containers()).await {
        Ok(container_services) => {
            // Also filter any container service that happens to share a PID
            // with a management-registered process.
            for svc in container_services {
                if !registered_pids.contains(&svc.pid) {
                    out.push(svc);
                }
            }
        }
        Err(_) => tracing::debug!("Docker network scan timed out"),
    }

    append_registered_network_services(&mut out, &registrations);

    dedupe_network_services(out)
}

/// Fast overlay-only scan used on first DNS hit for an unknown
/// `*.portzero.local` name.
///
/// This intentionally skips Docker and issue detection so a browser/curl lookup
/// can be satisfied as soon as the local process appears, while the normal
/// periodic scan still performs the full reconciliation.
pub async fn scan_network_process_services(
    mgmt_store: &crate::management::RegistrationStore,
) -> Option<Vec<DiscoveredNetworkService>> {
    let registrations = mgmt_store.read().await.clone();
    let registered_pids: std::collections::HashSet<u32> = registrations.keys().copied().collect();

    let mut out = match tokio::time::timeout(
        std::time::Duration::from_millis(750),
        scan_network_processes(&registered_pids),
    )
    .await
    {
        Ok(process_services) => process_services,
        Err(_) => {
            tracing::debug!("fast overlay process scan timed out");
            return None;
        }
    };

    append_registered_network_services(&mut out, &registrations);
    Some(dedupe_network_services(out))
}

fn append_registered_network_services(
    out: &mut Vec<DiscoveredNetworkService>,
    registrations: &std::collections::HashMap<u32, Vec<crate::management::PortRegistration>>,
) {
    // Synthesize overlay entries for management-registered PIDs that are still
    // alive. These bypass the PZ_TUNNEL env-var path and protocol probing.
    for (pid, regs) in registrations {
        if !crate::management::pid_lookup::pid_is_alive(*pid) {
            continue;
        }
        for reg in regs {
            if !is_local_overlay_domain(&reg.domain) {
                continue;
            }
            out.push(DiscoveredNetworkService {
                name: extract_local_label(&reg.domain),
                domain_template: reg.domain.clone(),
                substitutions: BTreeMap::new(),
                real_addr: std::net::SocketAddr::from(([127, 0, 0, 1], reg.local_port)),
                service_port: 80,
                backend_protocol: Some(protocol_detect::Canonical::Http),
                pid: *pid,
                source: ServiceSource::Process { cwd: None },
                health_path: None,
            });
        }
    }
}

/// Key used to deduplicate network services discovered from processes/containers.
type NetworkServiceDedupeKey = (
    String,
    String,
    BTreeMap<String, String>,
    std::net::SocketAddr,
    u16,
    String,
);

fn dedupe_network_services(
    services: Vec<DiscoveredNetworkService>,
) -> Vec<DiscoveredNetworkService> {
    let mut deduped: std::collections::BTreeMap<NetworkServiceDedupeKey, DiscoveredNetworkService> =
        std::collections::BTreeMap::new();

    for svc in services {
        let key = dedupe_network_service_key(&svc);
        match deduped.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(svc);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // Keep the lowest PID so the dashboard remains stable across scans
                // when a process tree shares the same listening socket.
                if svc.pid < entry.get().pid {
                    entry.insert(svc);
                }
            }
        }
    }

    deduped.into_values().collect()
}

fn dedupe_network_service_key(svc: &DiscoveredNetworkService) -> NetworkServiceDedupeKey {
    (
        svc.name.clone(),
        svc.domain_template.clone(),
        svc.substitutions.clone(),
        svc.real_addr,
        svc.service_port,
        service_context_dedupe_key(svc),
    )
}

fn service_source_dedupe_key(source: &ServiceSource) -> String {
    match source {
        ServiceSource::Process { cwd } => format!("process:{cwd:?}"),
        ServiceSource::Container { id, .. } => format!("container:{id}"),
    }
}

fn service_context_dedupe_key(svc: &DiscoveredNetworkService) -> String {
    match &svc.source {
        ServiceSource::Process { cwd: Some(cwd) } => format!("process-cwd:{cwd:?}"),
        ServiceSource::Process { cwd: None } => format!("process-pid:{}", svc.pid),
        ServiceSource::Container { .. } => service_source_dedupe_key(&svc.source),
    }
}

fn sanitize_network_name(raw: &str) -> String {
    raw.split('.')
        .filter_map(|label| {
            let sanitized = label
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
                .trim_matches(|c: char| !c.is_ascii_alphanumeric())
                .to_ascii_lowercase()
                .chars()
                .take(63)
                .collect::<String>();
            (!sanitized.is_empty()).then_some(sanitized)
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn command_on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };

    #[cfg(windows)]
    let candidates: Vec<String> = {
        let pathext = std::env::var_os("PATHEXT")
            .and_then(|v| v.into_string().ok())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
        let has_ext = std::path::Path::new(program).extension().is_some();
        if has_ext {
            vec![program.to_string()]
        } else {
            pathext
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(|ext| format!("{program}{ext}"))
                .chain(std::iter::once(program.to_string()))
                .collect()
        }
    };

    #[cfg(not(windows))]
    let candidates: Vec<String> = vec![program.to_string()];

    std::env::split_paths(&paths).any(|dir| {
        candidates
            .iter()
            .any(|candidate| dir.join(candidate).is_file())
    })
}

#[cfg(test)]
mod tests;
