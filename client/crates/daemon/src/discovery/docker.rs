#[allow(unused_imports)]
use super::*;

pub(super) async fn scan_docker_containers(
    account_id: Option<&str>,
    username: Option<&str>,
) -> (Vec<DiscoveredService>, Vec<crate::notify::Issue>) {
    if !command_on_path("docker") {
        return (Vec::new(), Vec::new());
    }

    let acct = account_id.map(str::to_owned);
    let user = username.map(str::to_owned);
    match tokio::task::spawn_blocking(move || {
        scan_docker_containers_impl(acct.as_deref(), user.as_deref())
    })
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("spawn_blocking panicked")))
    {
        Ok(result) => result,
        Err(e) => {
            tracing::debug!(
                "Docker container scan failed (Docker may not be running): {}",
                e
            );
            (Vec::new(), Vec::new())
        }
    }
}

/// Hard ceiling on any docker CLI invocation made from the discovery loop.
///
/// `Command::output()` on a docker child can block FOREVER: the child's stdout
/// pipe write-end can leak into the long-lived `docker events` stream child
/// (macOS lacks an atomic-CLOEXEC pipe, so concurrent spawns race), and then
/// the pipe never EOFs even after the child exits — wedging the whole
/// discovery loop. Every docker call below therefore runs under this watchdog
/// and is killed on overrun.
const DOCKER_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Run `docker <args>` with output capture, bounded by
/// [`DOCKER_COMMAND_TIMEOUT`]. On timeout the child is killed and an error
/// naming `label` is returned.
fn run_docker_capture(args: &[&str], label: &str) -> Result<std::process::Output> {
    let mut command = std::process::Command::new("docker");
    command.args(args);
    crate::tls::trust::run_command_capture_with_timeout(command, DOCKER_COMMAND_TIMEOUT, label)
}

/// Batched-inspect format: `{{.Id}}` first (so each output line self-identifies
/// its container), then exactly the fields [`parse_container_inspect_fields`]
/// expects.
const INSPECT_BATCH_FORMAT: &str = "{{.Id}}||{{json .Config.Env}}||{{.Name}}||{{json .NetworkSettings.Ports}}||{{.State.Pid}}||{{json .Mounts}}||{{json .Config.Labels}}";

/// ONE bounded `docker inspect` for all `ids` at once, returning
/// `(container_id, inspect_fields_line)` pairs.
///
/// Batching matters: per-container inspects meant one subprocess per container
/// per scan pass (every 2s) — dozens of spawns that crawled on
/// container-heavy machines and multiplied exposure to the leaked-pipe wedge
/// (see [`DOCKER_COMMAND_TIMEOUT`]). Containers that vanish between `ps` and
/// `inspect` are simply absent from the result: docker reports them on stderr
/// with a non-zero exit but still prints every found container on stdout, so
/// stdout is parsed regardless of exit status.
fn batch_inspect_lines(ids: &[&str]) -> Vec<(String, String)> {
    if ids.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["inspect", "--format", INSPECT_BATCH_FORMAT];
    args.extend_from_slice(ids);
    let output = match run_docker_capture(&args, "docker inspect (batched)") {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("batched docker inspect failed/timed out, skipping scan: {e:#}");
            return Vec::new();
        }
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once("||")
                .map(|(id, rest)| (id.to_string(), rest.to_string()))
        })
        .collect()
}

fn scan_docker_containers_impl(
    account_id: Option<&str>,
    username: Option<&str>,
) -> Result<(Vec<DiscoveredService>, Vec<crate::notify::Issue>)> {
    let output = run_docker_capture(&["ps", "--format", "{{.ID}}"], "docker ps")?;

    if !output.status.success() {
        anyhow::bail!("docker ps failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let container_ids: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    if container_ids.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut services = Vec::new();
    let mut issues = Vec::new();
    for (container_id, inspect_line) in batch_inspect_lines(&container_ids) {
        if let Some(svc) = container_service_from_inspect_line(
            &container_id,
            &inspect_line,
            account_id,
            username,
            &mut issues,
        )? {
            services.push(svc);
        }
    }

    Ok((services, issues))
}

/// Build a [`DiscoveredService`] from one container's pre-fetched
/// batched-inspect fields line (everything after the leading `{{.Id}}||`).
fn container_service_from_inspect_line(
    container_id: &str,
    inspect_line: &str,
    account_id: Option<&str>,
    username: Option<&str>,
    issues: &mut Vec<crate::notify::Issue>,
) -> Result<Option<DiscoveredService>> {
    let fields = match parse_container_inspect_fields(inspect_line) {
        Some(f) => f,
        None => return Ok(None),
    };

    let name = fields.name;
    let ports_json = fields.ports_json;
    let container_pid = fields.pid;
    let mounts_json = fields.mounts_json;
    let labels_json = fields.labels_json;

    let env_vars: Vec<String> = serde_json::from_str(fields.env_json).unwrap_or_default();

    let tunnel_prefix = format!("{ENV_VAR_NAME}=");
    let raw = env_vars
        .iter()
        .find_map(|e| e.strip_prefix(&tunnel_prefix))
        .map(|v| v.to_string());

    let raw = match raw {
        Some(d) if !d.is_empty() => d,
        _ => return Ok(None),
    };

    // Discover a host-side project directory for template resolution (branch, worktree, etc.)
    // This makes PZ_TUNNEL=my-service-{branch} work for `docker run` (via bind mounts)
    // and `docker compose` (via labels + mounts).
    let project_dir = find_host_project_dir_for_container(mounts_json, labels_json);

    // Strip an optional canonical `:port` before classification/validation.
    // On the cloud path the edge assigns the URL, so the port is ignored.
    let (raw_domain, canonical_port) = split_tunnel_port(&raw);
    warn_if_port_like_rejected(&raw, canonical_port, container_pid);

    let is_local = is_local_overlay_domain(raw_domain);
    if let Err(e) =
        portzero_domain::validate_username_placeholders(raw_domain, is_local, username.is_some())
    {
        tracing::warn!(
            container = name,
            template = raw_domain,
            error = %e,
            "PZ_TUNNEL template misuses {{local-username}}/{{cloud-username}}"
        );
        issues.push(crate::notify::Issue::InvalidUsernamePlaceholder {
            template: raw_domain.to_string(),
            reason: e,
            context: format!("container {name}"),
            requires_login: is_local,
            pid: Some(container_pid).filter(|&p| p != 0),
        });
        return Ok(None);
    }

    let substitutions =
        template_substitutions(project_dir.as_deref(), account_id, username, Some(name));
    let domain = resolve_tunnel_template(raw_domain, project_dir.as_deref(), account_id, username);

    if is_local_overlay_domain(&domain) {
        tracing::debug!(
            container = name,
            domain,
            "value ends in .local — routing to overlay path"
        );
        return Ok(None);
    }

    // Fail cleanly on unresolved tokens ({pr}/{run-id} outside CI) or an
    // ambiguous internal `--` in a label, rather than registering a garbled name.
    if let Err(e) = portzero_domain::validate_resolved_name(&domain) {
        tracing::warn!(
            container = name,
            template = raw_domain,
            domain,
            error = %e,
            "PZ_TUNNEL template resolved to an invalid name"
        );
        issues.push(crate::notify::Issue::InvalidResolvedName {
            template: raw_domain.to_string(),
            resolved: domain.clone(),
            reason: e,
            context: format!("container {name}"),
            pid: Some(container_pid).filter(|&p| p != 0),
        });
        return Ok(None);
    }

    if let Err(e) = validate_tunnel_domain(&domain) {
        tracing::warn!(
            container = name,
            domain,
            error = %e,
            "PZ_TUNNEL value on container is not a valid full tunnel domain (and not .local). \
             Use a full name like web-mybranch--alice.tunnel.portzero.cloud"
        );
        issues.push(crate::notify::Issue::InvalidCloudTunnelScope {
            domain,
            reason: e,
            context: format!("container {name}"),
            pid: Some(container_pid).filter(|&p| p != 0),
        });
        return Ok(None);
    }

    let http_selection = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("PZ_TUNNEL_HTTP_PORT="))
        .map(parse_http_port_selection)
        .unwrap_or(HttpPortSelection::ChooseLowest);

    let extra_ports = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("PZ_TUNNEL_PORTS="))
        .map(parse_extra_ports)
        .unwrap_or_default();

    let health_path = env_vars
        .iter()
        .find_map(|e| e.strip_prefix("PZ_HEALTH_PATH="))
        .and_then(normalize_health_path);

    let port = match http_selection {
        HttpPortSelection::Explicit(p) => p,
        _ => parse_docker_ports(ports_json, &http_selection),
    };

    Ok(Some(DiscoveredService {
        domain,
        domain_template: raw_domain.to_string(),
        substitutions,
        port,
        extra_ports,
        health_path,
        pid: container_pid,
        source: ServiceSource::Container {
            id: container_id.to_string(),
            name: name.to_string(),
        },
    }))
}

/// The `||`-separated fields printed by
/// `docker inspect --format "{{json .Config.Env}}||{{.Name}}||{{json .NetworkSettings.Ports}}||{{.State.Pid}}||{{json .Mounts}}||{{json .Config.Labels}}"`.
///
/// Borrowed from the raw `stdout` string so callers can deserialize
/// `env_json`/`ports_json`/`mounts_json`/`labels_json` as (and only if)
/// needed.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ContainerInspectFields<'a> {
    pub(super) env_json: &'a str,
    pub(super) name: &'a str,
    pub(super) ports_json: &'a str,
    pub(super) pid: u32,
    pub(super) mounts_json: &'a str,
    pub(super) labels_json: &'a str,
}

/// Parse one line of `docker inspect --format "...||...||...||...||...||..."`
/// output into its component fields. Pure string splitting so it is
/// unit-testable without shelling out to `docker`; used by both
/// `inspect_container` and `scan_network_containers_impl`, which previously
/// duplicated this logic.
///
/// Returns `None` when the format is malformed (fewer than the 4 required
/// `||`-separated fields) or when `State.Pid` is `0` — the container is
/// stopped/exited, which can happen in a race between `docker ps` (lists
/// running IDs) and `docker inspect` (runs slightly later, after the
/// container has already stopped). The trailing `Mounts`/`Labels` fields are
/// optional and default to `"[]"`/`"{}"` when the output was truncated to
/// fewer than 6 fields.
pub(super) fn parse_container_inspect_fields(stdout: &str) -> Option<ContainerInspectFields<'_>> {
    let stdout = stdout.trim();
    let parts: Vec<&str> = stdout.splitn(6, "||").collect();
    if parts.len() < 4 {
        return None;
    }

    let env_json = parts[0];
    let name = parts[1].trim_start_matches('/');
    let ports_json = parts[2];
    let pid: u32 = parts[3].parse().unwrap_or(0);
    let mounts_json = if parts.len() > 4 { parts[4] } else { "[]" };
    let labels_json = if parts.len() > 5 { parts[5] } else { "{}" };

    if pid == 0 {
        return None;
    }

    Some(ContainerInspectFields {
        env_json,
        name,
        ports_json,
        pid,
        mounts_json,
        labels_json,
    })
}

pub(super) fn parse_docker_ports(ports_json: &str, selection: &HttpPortSelection) -> u16 {
    let ports: serde_json::Value = match serde_json::from_str(ports_json) {
        Ok(v) => v,
        Err(_) => return 0,
    };

    let obj = match ports.as_object() {
        Some(o) => o,
        None => return 0,
    };

    let mut found: Vec<u16> = Vec::new();
    for (_container_port, bindings) in obj {
        if let Some(arr) = bindings.as_array() {
            for binding in arr {
                if let Some(port_str) = binding.get("HostPort").and_then(|v| v.as_str()) {
                    if let Ok(port) = port_str.parse::<u16>() {
                        if port > 0 {
                            found.push(port);
                        }
                    }
                }
            }
        }
    }

    match selection {
        HttpPortSelection::ChooseHighest => found.into_iter().max().unwrap_or(0),
        _ => found.into_iter().min().unwrap_or(0),
    }
}

/// Try to recover a host-side git project directory from a container's mounts
/// and labels. This enables correct `{branch}` / `{worktree}` resolution for
/// `PZ_TUNNEL` when using plain `docker run -v ...` or `docker compose`.
fn find_host_project_dir_for_container(mounts_json: &str, labels_json: &str) -> Option<PathBuf> {
    use portzero_domain::find_git_project_dir;

    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. Docker Compose working dir label (very reliable when using compose)
    if let Ok(labels) = serde_json::from_str::<serde_json::Value>(labels_json) {
        if let Some(wd) = labels
            .get("com.docker.compose.project.working_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
        {
            candidates.push(wd);
        }
        // Also check other common labels users might set
        if let Some(custom) = labels
            .get("dev.portzero.project_dir")
            .and_then(|v| v.as_str())
        {
            candidates.push(PathBuf::from(custom));
        }
    }

    // 2. Bind mounts from the host (works for both compose and plain docker run -v)
    if let Ok(mounts) = serde_json::from_str::<serde_json::Value>(mounts_json) {
        if let Some(arr) = mounts.as_array() {
            for mount in arr {
                if mount.get("Type").and_then(|t| t.as_str()) == Some("bind") {
                    if let Some(source) = mount.get("Source").and_then(|s| s.as_str()) {
                        candidates.push(PathBuf::from(source));
                    }
                }
            }
        }
    }

    // Convert to slices for the domain helper
    let refs: Vec<&Path> = candidates.iter().map(|p| p.as_path()).collect();
    find_git_project_dir(&refs)
}

pub(super) async fn scan_network_containers() -> Vec<DiscoveredNetworkService> {
    let mut services = match tokio::task::spawn_blocking(scan_network_containers_impl)
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("spawn_blocking panicked")))
    {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("Docker network scan failed: {}", e);
            return Vec::new();
        }
    };

    for svc in &mut services {
        if svc.service_port == 443 {
            svc.backend_protocol = protocol_detect::detect_canonical_cached(
                svc.real_addr,
                (svc.pid, svc.real_addr.port()),
            )
            .await;
        }
    }

    services
}

fn scan_network_containers_impl() -> anyhow::Result<Vec<DiscoveredNetworkService>> {
    use std::net::{IpAddr, SocketAddr};

    if !command_on_path("docker") {
        return Ok(Vec::new());
    }

    let output = run_docker_capture(&["ps", "--format", "{{.ID}}"], "docker ps")?;

    if !output.status.success() {
        anyhow::bail!("docker ps failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let ids: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    let mut out = Vec::new();

    for (id, inspect_line) in batch_inspect_lines(&ids) {
        let fields = match parse_container_inspect_fields(&inspect_line) {
            Some(f) => f,
            None => continue,
        };

        let _name = fields.name;
        let ports_json = fields.ports_json;
        let pid = fields.pid;
        let mounts_json = fields.mounts_json;
        let labels_json = fields.labels_json;

        let envs: Vec<String> = match serde_json::from_str(fields.env_json) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only PZ_TUNNEL is used. Overlay participation requires the value
        // to end with .portzero.local (pure suffix-based detection).
        let tunnel_prefix = format!("{ENV_VAR_NAME}=");
        let raw_name = envs
            .iter()
            .find_map(|e| e.strip_prefix(&tunnel_prefix))
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let raw_name = match raw_name {
            Some(s) => s,
            None => continue,
        };

        // Use host-side git context (mounts/labels) for templating.
        // Strip an optional canonical `:port` (the VIP listen port) first.
        let (raw_domain, canonical_port) = split_tunnel_port(&raw_name);
        warn_if_port_like_rejected(&raw_name, canonical_port, pid);
        let project_dir = find_host_project_dir_for_container(mounts_json, labels_json);
        let substitutions = template_substitutions(project_dir.as_deref(), None, None, Some(_name));
        let resolved_name = resolve_tunnel_template(raw_domain, project_dir.as_deref(), None, None);

        if !is_local_overlay_domain(&resolved_name) {
            continue;
        }

        if resolved_name.eq_ignore_ascii_case("portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid
            );
            continue;
        }

        if resolved_name.eq_ignore_ascii_case("api.portzero.local") {
            tracing::warn!(
                "PZ_TUNNEL=api.portzero.local is reserved by the portzero daemon; ignoring process {}",
                pid
            );
            continue;
        }

        // Skip local overlay tunnels whose resolved name still carries an
        // unresolved token ({pr}/{run-id} outside CI) or an ambiguous internal
        // `--`, rather than registering a garbled overlay name.
        if let Err(e) = portzero_domain::validate_resolved_name(&resolved_name) {
            tracing::warn!(
                container = _name,
                template = raw_domain,
                resolved_name,
                error = %e,
                "PZ_TUNNEL .local template resolved to an invalid name; skipping"
            );
            continue;
        }

        let label = extract_local_label(&resolved_name);
        if label.is_empty() {
            continue;
        }

        // Parse docker ports to find a (container_port -> host_port) pair.
        // We want the container port as service_port.
        let (service_port, host_port) = parse_docker_port_mapping(ports_json).unwrap_or((0, 0));
        if host_port == 0 {
            // No published ports or still port 0 not yet assigned.
            continue;
        }

        let real_addr = SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), host_port);
        // An explicit canonical port wins; otherwise prefer the container port,
        // falling back to the host port.
        let svc_port = canonical_port.unwrap_or(if service_port > 0 {
            service_port
        } else {
            host_port
        });

        let health_path = envs
            .iter()
            .find_map(|e| e.strip_prefix("PZ_HEALTH_PATH="))
            .and_then(normalize_health_path);

        let container_name = label.clone();
        out.push(DiscoveredNetworkService {
            name: label,
            domain_template: raw_domain.to_string(),
            substitutions,
            real_addr,
            service_port: svc_port,
            backend_protocol: None,
            pid,
            source: ServiceSource::Container {
                id: id.to_string(),
                name: container_name,
            },
            health_path,
        });
    }

    Ok(out)
}

fn parse_docker_port_mapping(ports_json: &str) -> Option<(u16, u16)> {
    let val: serde_json::Value = serde_json::from_str(ports_json).ok()?;
    let obj = val.as_object()?;
    for (container_port_key, bindings) in obj {
        // container_port_key looks like "5432/tcp"
        let container_port: u16 = container_port_key
            .split('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if let Some(arr) = bindings.as_array() {
            for b in arr {
                if let Some(hp) = b
                    .get("HostPort")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u16>().ok())
                {
                    if hp > 0 {
                        return Some((container_port, hp));
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // parse_container_inspect_fields
    // -----------------------------------------------------------------

    #[test]
    fn parse_container_inspect_fields_full_line() {
        let stdout = r#"["PZ_TUNNEL=api.portzero.local","PATH=/usr/bin"]||/my-container||{"80/tcp":[{"HostPort":"8080"}]}||4242||[{"Type":"bind","Source":"/host/repo"}]||{"dev.portzero.project_dir":"/host/repo"}"#;
        let fields = parse_container_inspect_fields(stdout).expect("should parse");
        assert_eq!(
            fields.env_json,
            r#"["PZ_TUNNEL=api.portzero.local","PATH=/usr/bin"]"#
        );
        // Leading '/' from `docker inspect`'s .Name is trimmed.
        assert_eq!(fields.name, "my-container");
        assert_eq!(fields.ports_json, r#"{"80/tcp":[{"HostPort":"8080"}]}"#);
        assert_eq!(fields.pid, 4242);
        assert_eq!(
            fields.mounts_json,
            r#"[{"Type":"bind","Source":"/host/repo"}]"#
        );
        assert_eq!(
            fields.labels_json,
            r#"{"dev.portzero.project_dir":"/host/repo"}"#
        );
    }

    #[test]
    fn parse_container_inspect_fields_trims_surrounding_whitespace() {
        let stdout = "  []||/c||{}||99||[]||{}  \n";
        let fields = parse_container_inspect_fields(stdout).expect("should parse");
        assert_eq!(fields.name, "c");
        assert_eq!(fields.pid, 99);
    }

    #[test]
    fn parse_container_inspect_fields_defaults_missing_optional_trailing_fields() {
        // Only the required 4 fields present — Mounts/Labels are omitted.
        let stdout = "[]||/c||{}||123";
        let fields = parse_container_inspect_fields(stdout).expect("should parse");
        assert_eq!(fields.mounts_json, "[]");
        assert_eq!(fields.labels_json, "{}");
    }

    #[test]
    fn parse_container_inspect_fields_pid_zero_is_none() {
        // State.Pid == 0 means the container has already stopped/exited.
        let stdout = "[]||/c||{}||0||[]||{}";
        assert!(parse_container_inspect_fields(stdout).is_none());
    }

    #[test]
    fn parse_container_inspect_fields_non_numeric_pid_defaults_to_zero_and_is_none() {
        let stdout = "[]||/c||{}||not-a-pid||[]||{}";
        assert!(parse_container_inspect_fields(stdout).is_none());
    }

    #[test]
    fn parse_container_inspect_fields_too_few_fields_is_none() {
        // Fewer than 4 `||`-separated fields — malformed/truncated output.
        assert!(parse_container_inspect_fields("[]||/c||{}").is_none());
        assert!(parse_container_inspect_fields("").is_none());
        assert!(parse_container_inspect_fields("garbage with no separators").is_none());
    }

    // -----------------------------------------------------------------
    // parse_docker_ports
    // -----------------------------------------------------------------

    #[test]
    fn parse_docker_ports_picks_lowest_across_multiple_container_ports() {
        let json = r#"{"3000/tcp":[{"HostIp":"0.0.0.0","HostPort":"40001"}],"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"30000"}]}"#;
        assert_eq!(
            parse_docker_ports(json, &HttpPortSelection::ChooseLowest),
            30000
        );
    }

    #[test]
    fn parse_docker_ports_picks_highest_across_multiple_bindings_same_port() {
        let json = r#"{"8080/tcp":[{"HostPort":"1000"},{"HostPort":"2000"}]}"#;
        assert_eq!(
            parse_docker_ports(json, &HttpPortSelection::ChooseHighest),
            2000
        );
    }

    #[test]
    fn parse_docker_ports_malformed_json_returns_zero() {
        assert_eq!(
            parse_docker_ports("not json", &HttpPortSelection::ChooseLowest),
            0
        );
        assert_eq!(parse_docker_ports("", &HttpPortSelection::ChooseLowest), 0);
        // A JSON array (not an object) at the top level.
        assert_eq!(
            parse_docker_ports("[1,2,3]", &HttpPortSelection::ChooseLowest),
            0
        );
    }

    #[test]
    fn parse_docker_ports_ignores_unparseable_or_zero_host_ports() {
        // HostPort missing, non-numeric, and zero must all be skipped —
        // leaving only the one valid binding.
        let json = r#"{"3000/tcp":[{"HostPort":"not-a-port"}],"4000/tcp":[{}],"5000/tcp":[{"HostPort":"0"}],"6000/tcp":[{"HostPort":"6000"}]}"#;
        assert_eq!(
            parse_docker_ports(json, &HttpPortSelection::ChooseLowest),
            6000
        );
    }

    #[test]
    fn parse_docker_ports_explicit_selection_still_scans_all_and_defaults_to_lowest() {
        // `HttpPortSelection::Explicit` is handled by the caller before this
        // function runs (see `inspect_container`); when reached here anyway
        // (the non-`ChooseHighest` arm) it behaves like `ChooseLowest`.
        let json = r#"{"3000/tcp":[{"HostPort":"9000"}],"8080/tcp":[{"HostPort":"8000"}]}"#;
        assert_eq!(
            parse_docker_ports(json, &HttpPortSelection::Explicit(3000)),
            8000
        );
    }

    // -----------------------------------------------------------------
    // parse_docker_port_mapping
    // -----------------------------------------------------------------

    #[test]
    fn parse_docker_port_mapping_typical() {
        let json = r#"{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"54320"}]}"#;
        assert_eq!(parse_docker_port_mapping(json), Some((5432, 54320)));
    }

    #[test]
    fn parse_docker_port_mapping_container_port_key_missing_slash_defaults_to_zero() {
        let json = r#"{"weird-key":[{"HostPort":"1234"}]}"#;
        assert_eq!(parse_docker_port_mapping(json), Some((0, 1234)));
    }

    #[test]
    fn parse_docker_port_mapping_skips_zero_host_port_and_missing_hostport() {
        let json = r#"{"80/tcp":[{"HostIp":"0.0.0.0","HostPort":"0"}],"443/tcp":[{}],"8080/tcp":[{"HostPort":"8080"}]}"#;
        // Iteration order over a JSON object isn't guaranteed by serde_json's
        // default map, but only one candidate here has a nonzero HostPort, so
        // the result is deterministic regardless of iteration order.
        assert_eq!(parse_docker_port_mapping(json), Some((8080, 8080)));
    }

    #[test]
    fn parse_docker_port_mapping_no_published_ports() {
        assert_eq!(parse_docker_port_mapping("{}"), None);
        assert_eq!(parse_docker_port_mapping("null"), None);
        assert_eq!(parse_docker_port_mapping(r#"{"80/tcp":null}"#), None);
    }

    #[test]
    fn parse_docker_port_mapping_malformed_json() {
        assert_eq!(parse_docker_port_mapping("not json"), None);
        assert_eq!(parse_docker_port_mapping(""), None);
        assert_eq!(parse_docker_port_mapping("[1,2,3]"), None);
    }

    // -----------------------------------------------------------------
    // find_host_project_dir_for_container
    // -----------------------------------------------------------------

    #[test]
    fn find_host_project_dir_for_container_from_compose_label() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

        let labels = serde_json::json!({
            "com.docker.compose.project.working_dir": tmp.path().to_string_lossy(),
        })
        .to_string();

        let found = find_host_project_dir_for_container("[]", &labels).expect("should find repo");
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn find_host_project_dir_for_container_from_custom_label() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

        let labels = serde_json::json!({
            "dev.portzero.project_dir": tmp.path().to_string_lossy(),
        })
        .to_string();

        let found = find_host_project_dir_for_container("[]", &labels).expect("should find repo");
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn find_host_project_dir_for_container_from_bind_mount() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

        let mounts = serde_json::json!([
            {"Type": "volume", "Source": "some-named-volume"},
            {"Type": "bind", "Source": tmp.path().to_string_lossy()},
        ])
        .to_string();

        let found = find_host_project_dir_for_container(&mounts, "{}")
            .expect("should find repo via bind mount");
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn find_host_project_dir_for_container_label_takes_precedence_over_mount() {
        let label_repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(label_repo.path().join(".git")).expect("mkdir .git");
        let mount_repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(mount_repo.path().join(".git")).expect("mkdir .git");

        let labels = serde_json::json!({
            "com.docker.compose.project.working_dir": label_repo.path().to_string_lossy(),
        })
        .to_string();
        let mounts = serde_json::json!([
            {"Type": "bind", "Source": mount_repo.path().to_string_lossy()},
        ])
        .to_string();

        let found =
            find_host_project_dir_for_container(&mounts, &labels).expect("should find a repo");
        assert_eq!(found, label_repo.path());
    }

    #[test]
    fn find_host_project_dir_for_container_ignores_non_bind_mounts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("mkdir .git");

        // A "volume" mount pointing at a real git repo must NOT be used —
        // only "bind" mounts carry a meaningful host path.
        let mounts = serde_json::json!([
            {"Type": "volume", "Source": tmp.path().to_string_lossy()},
        ])
        .to_string();

        assert!(find_host_project_dir_for_container(&mounts, "{}").is_none());
    }

    #[test]
    fn find_host_project_dir_for_container_malformed_json_yields_none() {
        assert!(find_host_project_dir_for_container("not json", "not json either").is_none());
        assert!(find_host_project_dir_for_container("", "").is_none());
    }

    #[test]
    fn find_host_project_dir_for_container_no_git_repo_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // No .git directory created — not a repo.
        let mounts = serde_json::json!([
            {"Type": "bind", "Source": tmp.path().to_string_lossy()},
        ])
        .to_string();
        assert!(find_host_project_dir_for_container(&mounts, "{}").is_none());
    }
}
