//! Scanning processes for PZ_TUNNEL, both for cloud tunnels (`scan_processes`)
//! and the local `.portzero.local` overlay (`scan_network_processes`).

#[allow(unused_imports)]
use super::*;

mod context;
mod platform;

#[allow(unused_imports)]
pub(in crate::discovery) use context::*;
#[allow(unused_imports)]
pub(in crate::discovery) use platform::*;

pub(super) fn scan_processes(
    account_id: Option<&str>,
    username: Option<&str>,
) -> (Vec<DiscoveredService>, Vec<crate::notify::Issue>) {
    #[cfg(target_os = "windows")]
    {
        scan_processes_windows(account_id, username)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let mut services = Vec::new();
        let mut issues = Vec::new();

        // (pid, PZ_TUNNEL) candidates. macOS reads each process's env via
        // sysctl(KERN_PROCARGS2) rather than `ps -E`, which stopped exposing
        // process environments on macOS 15.7+ (task-74; see
        // scan_process_env_macos). KERN_PROCARGS2 is a syscall with no
        // subprocess, so the per-pid scan is cheap — like Linux reading
        // /proc/<pid>/environ. Mirrors scan_network_processes_sync_macos.
        #[cfg(target_os = "macos")]
        let candidates: Vec<(u32, String)> = scan_macos_env_candidates(&sys);
        #[cfg(not(target_os = "macos"))]
        let candidates: Vec<(u32, String)> = sys
            .processes()
            .keys()
            .filter_map(|pid| {
                let pid_u32 = pid.as_u32();
                match scan_process_env(pid_u32, ENV_VAR_NAME) {
                    Some(v) if !v.is_empty() => Some((pid_u32, v)),
                    _ => None,
                }
            })
            .collect();

        for (pid_u32, raw) in candidates {
            if pid_u32 <= 1 {
                continue;
            }

            let process_context = process_template_context(&sys, pid_u32);
            // Strip an optional canonical `:port` before classification/validation.
            // On the cloud path the edge assigns the URL, so the port is ignored.
            let (raw_domain, _canonical_port) = split_tunnel_port(&raw);
            warn_if_port_like_rejected(&raw, _canonical_port, pid_u32);

            let is_local = is_local_overlay_domain(raw_domain);
            if let Err(e) = portzero_domain::validate_username_placeholders(
                raw_domain,
                is_local,
                username.is_some(),
            ) {
                tracing::warn!(
                    pid = pid_u32,
                    template = raw_domain,
                    error = %e,
                    "PZ_TUNNEL template misuses {{local-username}}/{{cloud-username}}"
                );
                issues.push(crate::notify::Issue::InvalidUsernamePlaceholder {
                    template: raw_domain.to_string(),
                    reason: e,
                    context: format!("pid {pid_u32}"),
                    requires_login: is_local,
                    pid: Some(pid_u32),
                });
                continue;
            }

            let substitutions = template_substitutions(
                process_context.project_dir.as_deref(),
                account_id,
                username,
                None,
            );
            let domain = resolve_tunnel_template(
                raw_domain,
                process_context.project_dir.as_deref(),
                account_id,
                username,
            );

            if is_local_overlay_domain(&domain) {
                tracing::debug!(
                    pid = pid_u32,
                    domain,
                    "PZ_TUNNEL value ends in .local — routing to overlay path"
                );
                continue;
            }

            // Fail cleanly on unresolved tokens ({pr}/{run-id} outside CI) or an
            // ambiguous internal `--` in a label, rather than registering a
            // garbled cloud tunnel name.
            if let Err(e) = portzero_domain::validate_resolved_name(&domain) {
                tracing::warn!(
                    pid = pid_u32,
                    template = raw_domain,
                    domain,
                    error = %e,
                    "PZ_TUNNEL template resolved to an invalid name"
                );
                issues.push(crate::notify::Issue::InvalidResolvedName {
                    template: raw_domain.to_string(),
                    resolved: domain.clone(),
                    reason: e,
                    context: format!("pid {pid_u32}"),
                    pid: Some(pid_u32),
                });
                continue;
            }

            // For cloud tunnels we require a full domain name (no implicit suffix).
            if let Err(e) = validate_tunnel_domain(&domain) {
                tracing::warn!(
                    pid = pid_u32,
                    domain,
                    error = %e,
                    "PZ_TUNNEL value is not a valid full tunnel domain (and not .local). \
                     Provide the full name including suffix, e.g. my-api--alice.tunnel.portzero.cloud"
                );
                issues.push(crate::notify::Issue::InvalidCloudTunnelScope {
                    domain,
                    reason: e,
                    context: format!("pid {pid_u32}"),
                    pid: Some(pid_u32),
                });
                continue;
            }

            let http_selection = scan_process_env(pid_u32, ENV_HTTP_PORT_VAR)
                .map(|v| parse_http_port_selection(&v))
                .unwrap_or(HttpPortSelection::ChooseLowest);

            let extra_ports = scan_process_env(pid_u32, ENV_PORTS_VAR)
                .map(|v| parse_extra_ports(&v))
                .unwrap_or_default();

            let health_path = scan_process_env(pid_u32, ENV_HEALTH_PATH_VAR)
                .and_then(|v| normalize_health_path(&v));

            let listening = discover_process_ports(pid_u32);

            let port = match select_http_port(&listening, &http_selection) {
                SelectedPort::Found(p) => p,
                SelectedPort::ExplicitNotOwned(requested) => {
                    tracing::warn!(
                        pid = pid_u32,
                        requested_port = requested,
                        "PZ_TUNNEL_HTTP_PORT specifies a port not owned by this process; ignoring"
                    );
                    continue;
                }
                // No listening ports on this process. This is expected when a
                // parent shell or launcher sets PZ_TUNNEL so that a child
                // process inherits it — the parent itself has nothing to forward.
                SelectedPort::NoneListening => {
                    tracing::debug!(
                        pid = pid_u32,
                        "skipping process with PZ_TUNNEL but no listening ports \
                     (likely a parent process passing the variable to its child)"
                    );
                    continue;
                }
            };

            let owned_ports: std::collections::HashSet<u16> =
                listening.iter().map(|lp| lp.port).collect();
            let extra_ports: Vec<PortMapping> = extra_ports
            .into_iter()
            .filter(|m| {
                if owned_ports.contains(&m.local_port) {
                    true
                } else {
                    tracing::warn!(
                        pid = pid_u32,
                        local_port = m.local_port,
                        "PZ_TUNNEL_PORTS entry references a port not owned by this process; skipping"
                    );
                    false
                }
            })
            .collect();

            services.push(DiscoveredService {
                domain,
                domain_template: raw_domain.to_string(),
                substitutions,
                port,
                host: dial_host_for_port(&listening, port),
                extra_ports,
                health_path,
                pid: pid_u32,
                source: ServiceSource::Process {
                    cwd: process_context.cwd,
                },
            });
        }

        (services, issues)
    }
}

#[cfg(target_os = "windows")]
pub(super) fn scan_processes_windows(
    account_id: Option<&str>,
    username: Option<&str>,
) -> (Vec<DiscoveredService>, Vec<crate::notify::Issue>) {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let listening_by_pid = discover_all_ports_windows_by_pid();
    let mut services = Vec::new();
    let mut issues = Vec::new();

    for (pid_u32, listening) in listening_by_pid {
        if pid_u32 <= 1 || listening.is_empty() {
            continue;
        }

        let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        let process_context = process_template_context(&sys, pid_u32);
        let (raw_domain, _canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, _canonical_port, pid_u32);

        let is_local = is_local_overlay_domain(raw_domain);
        if let Err(e) = portzero_domain::validate_username_placeholders(
            raw_domain,
            is_local,
            username.is_some(),
        ) {
            tracing::warn!(
                pid = pid_u32,
                template = raw_domain,
                error = %e,
                "PZ_TUNNEL template misuses {{local-username}}/{{cloud-username}}"
            );
            issues.push(crate::notify::Issue::InvalidUsernamePlaceholder {
                template: raw_domain.to_string(),
                reason: e,
                context: format!("pid {pid_u32}"),
                requires_login: is_local,
                pid: Some(pid_u32),
            });
            continue;
        }

        let substitutions = template_substitutions(
            process_context.project_dir.as_deref(),
            account_id,
            username,
            None,
        );
        let domain = resolve_tunnel_template(
            raw_domain,
            process_context.project_dir.as_deref(),
            account_id,
            username,
        );

        if is_local_overlay_domain(&domain) {
            tracing::debug!(
                pid = pid_u32,
                domain,
                "PZ_TUNNEL value ends in .local — routing to overlay path"
            );
            continue;
        }

        // Fail cleanly on unresolved tokens ({pr}/{run-id} outside CI) or an
        // ambiguous internal `--` in a label.
        if let Err(e) = portzero_domain::validate_resolved_name(&domain) {
            tracing::warn!(
                pid = pid_u32,
                template = raw_domain,
                domain,
                error = %e,
                "PZ_TUNNEL template resolved to an invalid name"
            );
            issues.push(crate::notify::Issue::InvalidResolvedName {
                template: raw_domain.to_string(),
                resolved: domain.clone(),
                reason: e,
                context: format!("pid {pid_u32}"),
                pid: Some(pid_u32),
            });
            continue;
        }

        if let Err(e) = validate_tunnel_domain(&domain) {
            tracing::warn!(
                pid = pid_u32,
                domain,
                error = %e,
                "PZ_TUNNEL value is not a valid full tunnel domain (and not .local). \
                 Provide the full name including suffix, e.g. my-api--alice.tunnel.portzero.cloud"
            );
            issues.push(crate::notify::Issue::InvalidCloudTunnelScope {
                domain,
                reason: e,
                context: format!("pid {pid_u32}"),
                pid: Some(pid_u32),
            });
            continue;
        }

        let http_selection = scan_process_env(pid_u32, ENV_HTTP_PORT_VAR)
            .map(|v| parse_http_port_selection(&v))
            .unwrap_or(HttpPortSelection::ChooseLowest);

        let extra_ports = scan_process_env(pid_u32, ENV_PORTS_VAR)
            .map(|v| parse_extra_ports(&v))
            .unwrap_or_default();

        let health_path =
            scan_process_env(pid_u32, ENV_HEALTH_PATH_VAR).and_then(|v| normalize_health_path(&v));

        let port = match select_http_port(&listening, &http_selection) {
            SelectedPort::Found(p) => p,
            SelectedPort::ExplicitNotOwned(requested) => {
                tracing::warn!(
                    pid = pid_u32,
                    requested_port = requested,
                    "PZ_TUNNEL_HTTP_PORT specifies a port not owned by this process; ignoring"
                );
                continue;
            }
            SelectedPort::NoneListening => continue,
        };

        let owned_ports: std::collections::HashSet<u16> =
            listening.iter().map(|lp| lp.port).collect();
        let extra_ports: Vec<PortMapping> = extra_ports
            .into_iter()
            .filter(|m| {
                if owned_ports.contains(&m.local_port) {
                    true
                } else {
                    tracing::warn!(
                        pid = pid_u32,
                        local_port = m.local_port,
                        "PZ_TUNNEL_PORTS entry references a port not owned by this process; skipping"
                    );
                    false
                }
            })
            .collect();

        services.push(DiscoveredService {
            domain,
            domain_template: raw_domain.to_string(),
            substitutions,
            port,
            host: dial_host_for_port(&listening, port),
            extra_ports,
            health_path,
            pid: pid_u32,
            source: ServiceSource::Process {
                cwd: process_context.cwd,
            },
        });
    }

    (services, issues)
}

struct NetProcessCandidate {
    label: String,
    domain_template: String,
    substitutions: BTreeMap<String, String>,
    real_addr: std::net::SocketAddr,
    port: u16,
    needs_probe: bool,
    pid: u32,
    cwd: Option<PathBuf>,
    health_path: Option<String>,
}

// Blocking half: sysinfo refresh + per-process ps/lsof calls. Must not .await.
fn scan_network_processes_sync() -> Vec<NetProcessCandidate> {
    #[cfg(target_os = "windows")]
    {
        scan_network_processes_sync_windows()
    }

    #[cfg(target_os = "macos")]
    {
        scan_network_processes_sync_macos()
    }

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        use std::net::SocketAddr;

        let mut sys = System::new();
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let daemon_no_probe = std::env::var(ENV_NO_PROBE_VAR).ok();
        let mut candidates = Vec::new();

        for pid in sys.processes().keys() {
            let pid_u32 = pid.as_u32();
            if pid_u32 <= 1 {
                continue;
            }

            let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
                Some(v) if !v.is_empty() => v,
                _ => continue,
            };

            let process_context = process_template_context(&sys, pid_u32);
            let (raw_domain, canonical_port) = split_tunnel_port(&raw);
            warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
            let substitutions =
                template_substitutions(process_context.project_dir.as_deref(), None, None, None);
            let resolved = resolve_tunnel_template(
                raw_domain,
                process_context.project_dir.as_deref(),
                None,
                None,
            );

            if !is_local_overlay_domain(&resolved) {
                continue;
            }

            if resolved.eq_ignore_ascii_case("portzero.local") {
                tracing::warn!(
                    "PZ_TUNNEL=portzero.local is reserved by the portzero daemon; ignoring process {}",
                    pid_u32
                );
                continue;
            }

            if resolved.eq_ignore_ascii_case("api.portzero.local") {
                tracing::warn!(
                    "PZ_TUNNEL=api.portzero.local is reserved by the portzero daemon; ignoring process {}",
                    pid_u32
                );
                continue;
            }

            // Skip local overlay tunnels whose resolved name still carries an
            // unresolved token ({pr}/{run-id} outside CI) or an ambiguous
            // internal `--`, rather than registering a garbled overlay name.
            if let Err(e) = portzero_domain::validate_resolved_name(&resolved) {
                tracing::warn!(
                    pid = pid_u32,
                    template = raw_domain,
                    resolved,
                    error = %e,
                    "PZ_TUNNEL .local template resolved to an invalid name; skipping"
                );
                continue;
            }

            let label = extract_local_label(&resolved);
            if label.is_empty() {
                continue;
            }

            let listening = discover_process_ports(pid_u32);
            if listening.is_empty() {
                continue;
            }

            let chosen = listening
                .iter()
                .find(|lp| lp.is_wildcard())
                .or_else(|| listening.first());

            let (port, dial_host) = match chosen {
                Some(lp) => (lp.port, lp.dial_addr()),
                None => continue,
            };

            let real_addr = SocketAddr::new(dial_host, port);

            let (port, needs_probe) = match canonical_port {
                Some(explicit) => (explicit, false),
                None => {
                    let per_process = scan_process_env(pid_u32, ENV_NO_PROBE_VAR);
                    let no_probe = protocol_detect::probing_disabled(
                        per_process.as_deref(),
                        daemon_no_probe.as_deref(),
                    );
                    (port, !no_probe)
                }
            };

            candidates.push(NetProcessCandidate {
                label,
                domain_template: raw_domain.to_string(),
                substitutions,
                real_addr,
                port,
                needs_probe,
                pid: pid_u32,
                cwd: process_context.cwd,
                health_path: scan_process_env(pid_u32, ENV_HEALTH_PATH_VAR)
                    .and_then(|v| normalize_health_path(&v)),
            });
        }

        candidates
    }
}

#[cfg(target_os = "macos")]
fn scan_network_processes_sync_macos() -> Vec<NetProcessCandidate> {
    use std::net::{IpAddr, SocketAddr};

    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let daemon_no_probe = std::env::var(ENV_NO_PROBE_VAR).ok();
    let mut candidates = Vec::new();

    for (pid_u32, raw) in scan_macos_env_candidates(&sys) {
        if pid_u32 <= 1 {
            continue;
        }

        let process_context = process_template_context(&sys, pid_u32);
        let (raw_domain, canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
        let substitutions =
            template_substitutions(process_context.project_dir.as_deref(), None, None, None);
        let resolved = resolve_tunnel_template(
            raw_domain,
            process_context.project_dir.as_deref(),
            None,
            None,
        );

        if !is_local_overlay_domain(&resolved) {
            continue;
        }

        if resolved.eq_ignore_ascii_case("portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid_u32
            );
            continue;
        }

        if resolved.eq_ignore_ascii_case("api.portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=api.portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid_u32
            );
            continue;
        }

        // Skip local overlay tunnels whose resolved name still carries an
        // unresolved token ({pr}/{run-id} outside CI) or an ambiguous internal
        // `--`, rather than registering a garbled overlay name.
        if let Err(e) = portzero_domain::validate_resolved_name(&resolved) {
            tracing::warn!(
                pid = pid_u32,
                template = raw_domain,
                resolved,
                error = %e,
                "PZ_TUNNEL .local template resolved to an invalid name; skipping"
            );
            continue;
        }

        let label = extract_local_label(&resolved);
        if label.is_empty() {
            continue;
        }

        // We only invoke lsof for processes that actually advertise PZ_TUNNEL.
        // The previous macOS path invoked `ps` once per process and commonly
        // exceeded the overlay scan timeout before reaching the real service.
        let listening = discover_process_ports(pid_u32);
        if listening.is_empty() {
            continue;
        }

        let chosen = listening
            .iter()
            .find(|lp| lp.is_wildcard())
            .or_else(|| listening.first());

        let (port, dial_host) = match chosen {
            Some(lp) => (lp.port, lp.dial_addr()),
            None => continue,
        };

        let real_addr = SocketAddr::new(dial_host, port);

        let (port, needs_probe) = match canonical_port {
            Some(explicit) => (explicit, false),
            None => {
                let per_process = scan_process_env(pid_u32, ENV_NO_PROBE_VAR);
                let no_probe = protocol_detect::probing_disabled(
                    per_process.as_deref(),
                    daemon_no_probe.as_deref(),
                );
                (port, !no_probe)
            }
        };

        candidates.push(NetProcessCandidate {
            label,
            domain_template: raw_domain.to_string(),
            substitutions,
            real_addr,
            port,
            needs_probe,
            pid: pid_u32,
            cwd: process_context.cwd,
            health_path: scan_process_env(pid_u32, ENV_HEALTH_PATH_VAR)
                .and_then(|v| normalize_health_path(&v)),
        });
    }

    candidates
}

/// Collect `(pid, PZ_TUNNEL)` candidates for every process on the system.
///
/// This reads each process's env via sysctl(KERN_PROCARGS2) (see
/// scan_process_env_macos for why, not `ps -E`). Because KERN_PROCARGS2 is a
/// syscall with no subprocess, reading env per-pid is cheap — the single
/// batched `ps` call that this used to require for speed is no longer needed.
/// The old batched `ps -E` path is preserved in scan_macos_env_candidates_ps;
/// swap the active line below for the commented one to fall back to it.
#[cfg(target_os = "macos")]
fn scan_macos_env_candidates(sys: &System) -> Vec<(u32, String)> {
    sys.processes()
        .keys()
        .filter_map(|pid| {
            let pid_u32 = pid.as_u32();
            match scan_process_env(pid_u32, ENV_VAR_NAME) {
                Some(v) if !v.is_empty() => Some((pid_u32, v)),
                _ => None,
            }
        })
        .collect()
    // scan_macos_env_candidates_ps()
}

/// Old batched candidate collection via a single `ps -axo pid=,command= -wwwE`.
/// Broken on macOS 15.7+ (see scan_process_env_macos); kept as the documented
/// fallback that pairs with parse_macos_ps_env_candidates.
#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn scan_macos_env_candidates_ps() -> Vec<(u32, String)> {
    use std::process::Command;

    match Command::new("ps")
        .args(["-axo", "pid=,command=", "-wwwE"])
        .output()
    {
        Ok(o) if o.status.success() => {
            parse_macos_ps_env_candidates(&String::from_utf8_lossy(&o.stdout), ENV_VAR_NAME)
        }
        _ => Vec::new(),
    }
}

#[cfg(target_os = "windows")]
fn scan_network_processes_sync_windows() -> Vec<NetProcessCandidate> {
    use std::net::{IpAddr, SocketAddr};

    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let daemon_no_probe = std::env::var(ENV_NO_PROBE_VAR).ok();
    let listening_by_pid = discover_all_ports_windows_by_pid();
    let mut candidates = Vec::new();

    for (pid_u32, listening) in listening_by_pid {
        if pid_u32 <= 1 || listening.is_empty() {
            continue;
        }

        let raw = match scan_process_env(pid_u32, ENV_VAR_NAME) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        let process_context = process_template_context(&sys, pid_u32);
        let (raw_domain, canonical_port) = split_tunnel_port(&raw);
        warn_if_port_like_rejected(&raw, canonical_port, pid_u32);
        let substitutions =
            template_substitutions(process_context.project_dir.as_deref(), None, None, None);
        let resolved = resolve_tunnel_template(
            raw_domain,
            process_context.project_dir.as_deref(),
            None,
            None,
        );

        if !is_local_overlay_domain(&resolved) {
            continue;
        }

        if resolved.eq_ignore_ascii_case("portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid_u32
            );
            continue;
        }

        if resolved.eq_ignore_ascii_case("api.portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=api.portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid_u32
            );
            continue;
        }

        // Skip local overlay tunnels whose resolved name still carries an
        // unresolved token ({pr}/{run-id} outside CI) or an ambiguous internal
        // `--`, rather than registering a garbled overlay name.
        if let Err(e) = portzero_domain::validate_resolved_name(&resolved) {
            tracing::warn!(
                pid = pid_u32,
                template = raw_domain,
                resolved,
                error = %e,
                "PZ_TUNNEL .local template resolved to an invalid name; skipping"
            );
            continue;
        }

        let label = extract_local_label(&resolved);
        if label.is_empty() {
            continue;
        }

        let chosen = listening
            .iter()
            .find(|lp| lp.is_wildcard())
            .or_else(|| listening.first());

        let (port, dial_host) = match chosen {
            Some(lp) => (lp.port, lp.dial_addr()),
            None => continue,
        };

        let real_addr = SocketAddr::new(dial_host, port);

        let (port, needs_probe) = match canonical_port {
            Some(explicit) => (explicit, false),
            None => {
                let per_process = scan_process_env(pid_u32, ENV_NO_PROBE_VAR);
                let no_probe = protocol_detect::probing_disabled(
                    per_process.as_deref(),
                    daemon_no_probe.as_deref(),
                );
                (port, !no_probe)
            }
        };

        candidates.push(NetProcessCandidate {
            label,
            domain_template: raw_domain.to_string(),
            substitutions,
            real_addr,
            port,
            needs_probe,
            pid: pid_u32,
            cwd: process_context.cwd,
            health_path: scan_process_env(pid_u32, ENV_HEALTH_PATH_VAR)
                .and_then(|v| normalize_health_path(&v)),
        });
    }

    candidates
}

pub(super) async fn scan_network_processes(
    registered_pids: &std::collections::HashSet<u32>,
) -> Vec<DiscoveredNetworkService> {
    // Run the blocking scan (sysinfo + per-process ps/lsof) on the blocking
    // thread pool so the async executor is free to handle shutdown signals and
    // other tasks while the scan runs.
    let candidates = tokio::task::spawn_blocking(scan_network_processes_sync)
        .await
        .unwrap_or_default();

    let mut results = Vec::new();
    for c in candidates {
        // Management-registered PIDs are synthesized separately from the
        // registration store; skip them here to avoid double-registration.
        if registered_pids.contains(&c.pid) {
            continue;
        }

        // Precedence (task-16 + task-17):
        //   explicit `:port`  >  detected canonical (HTTP→80 / TLS→443)  >  ephemeral.
        let detected = if c.needs_probe || c.port == 443 {
            protocol_detect::detect_canonical_cached(c.real_addr, (c.pid, c.port)).await
        } else {
            None
        };

        let service_port = if c.needs_probe {
            detected
                .map(protocol_detect::Canonical::port)
                .unwrap_or(c.port)
        } else {
            c.port
        };

        results.push(DiscoveredNetworkService {
            name: c.label,
            domain_template: c.domain_template,
            substitutions: c.substitutions,
            real_addr: c.real_addr,
            service_port,
            backend_protocol: detected,
            pid: c.pid,
            source: ServiceSource::Process { cwd: c.cwd },
            health_path: c.health_path,
        });
    }

    results
}

#[cfg(test)]
mod tests;
