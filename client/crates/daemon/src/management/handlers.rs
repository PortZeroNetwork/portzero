//! Axum request handlers for the management API. The developer status
//! dashboard (the HTML shell, `/status.json`, and their helpers) lives in the
//! [`dashboard`] submodule.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::management::pid_lookup;
use crate::management::port_verify;
use crate::management::server::{AppState, PortRegistration};
use crate::route_table::OverlayState;

const MAX_REGISTRATIONS_PER_PROCESS: usize = 64;
/// Names a user may not register, shared with the discovery/auto-open guards so
/// there is a single source of truth for what the daemon reserves for itself.
use crate::discovery::RESERVED_OVERLAY_DOMAINS as RESERVED_REGISTRATION_NAMES;

mod dashboard;
pub use dashboard::{
    openapi_json, portzero_mark_asset, portzero_wordmark_asset, start_login, status_json, status_ui,
};

mod examples;
pub use examples::{
    download_examples, examples_status, run_example, spawn_auto_download, stop_example,
    ExamplesDownload, RunningExamples,
};

#[derive(Debug, Deserialize, ToSchema)]
pub struct RegisterRequest {
    /// One or more port→domain mappings to register. All existing
    /// registrations for this PID are replaced atomically.
    #[schema(min_items = 1, max_items = 64)]
    pub ports: Vec<PortRegistration>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RegisterResponse {
    /// PID the daemon resolved from the connection's TCP source port.
    #[schema(example = 12345)]
    pid: u32,
    /// Number of port→domain mappings that were successfully stored.
    #[schema(example = 2)]
    registered: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeregisterResponse {
    /// PID the daemon resolved from the connection's TCP source port.
    #[schema(example = 12345)]
    pid: u32,
    /// Always `true` on a 200 response.
    #[schema(example = true)]
    deregistered: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StatusResponse {
    /// PID the daemon resolved from the connection's TCP source port.
    #[schema(example = 12345)]
    pid: u32,
    /// Port→domain mappings currently registered for this process.
    ports: Vec<PortRegistration>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiRegistrationsStatus {
    /// Stability marker for the local API registration feature.
    #[schema(example = "unstable")]
    feature_stability: &'static str,
    /// Number of active port registrations created through the local API.
    #[schema(example = 2)]
    count: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HttpsStatus {
    /// Whether daemon-managed HTTPS is enabled for plaintext backends exposed on port 80.
    #[schema(example = true)]
    enabled: bool,
    /// Whether HTTP port 80 requests are redirected to HTTPS.
    #[schema(example = false)]
    redirect_port_80: bool,
    /// Whether TLS passthrough is enabled for HTTPS backends exposed on port 443.
    #[schema(example = true)]
    passthrough_port_443: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DaemonStatusResponse {
    /// Version of the running portzero daemon.
    #[schema(example = "0.1.0")]
    version: &'static str,
    /// Coarse daemon state for local app health checks.
    #[schema(example = "running")]
    status: &'static str,
    /// PID read from the daemon state directory, when available.
    #[schema(example = 12345)]
    daemon_pid: Option<u32>,
    /// Whether the local overlay is active.
    #[schema(example = true)]
    overlay_active: bool,
    /// Whether the daemon is connected for cloud tunnels.
    #[schema(example = false)]
    cloud_connected: bool,
    /// HTTPS behavior for *.portzero.local overlay routes.
    https: HttpsStatus,
    /// Summary of local API registrations. This feature is unstable.
    registrations: ApiRegistrationsStatus,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    /// Machine-readable error code.
    #[schema(examples("caller_unidentifiable", "port_not_listening", "not_registered"))]
    error: &'static str,
    /// Human-readable message with additional detail.
    detail: String,
}

/// Resolve the source port from the connecting address to a PID.
/// Returns an error response tuple on failure.
fn resolve_pid(addr: &SocketAddr) -> Result<u32, (StatusCode, Json<ErrorResponse>)> {
    let source_port = addr.port();
    match pid_lookup::pid_for_source_port(source_port) {
        Some(pid) => Ok(pid),
        None => {
            tracing::warn!(
                "management: could not identify caller on port {}",
                source_port
            );
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "caller_unidentifiable",
                    detail: format!("Could not identify the PID for source port {}", source_port),
                }),
            ))
        }
    }
}

/// Read daemon PID from `{state_dir}/daemon.pid`.  Returns `None` if missing/unreadable.
fn read_daemon_pid(state_dir: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(state_dir.join("daemon.pid"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

/// Read and parse `overlay.json`.  Returns a default (inactive, empty) state on any error.
fn read_overlay_state(state_dir: &std::path::Path) -> OverlayState {
    OverlayState::load(&state_dir.join("overlay.json")).unwrap_or_default()
}

/// Read `cloud_state.json` and extract connected + error + plan + can_use_cloud_tunnels +
/// message for upsells.
fn read_cloud_state(
    state_dir: &std::path::Path,
) -> (
    bool,
    Option<String>,
    Option<String>,
    Option<bool>,
    Option<String>,
) {
    let cloud_json_path = state_dir.join("cloud_state.json");
    let content = match std::fs::read_to_string(&cloud_json_path) {
        Ok(c) => c,
        Err(_) => return (false, None, None, None, None),
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return (false, None, None, None, None),
    };
    let connected = v
        .get("connected")
        .and_then(|c| c.as_bool())
        .unwrap_or(false);
    let error = v
        .get("error")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());
    let plan = v
        .get("plan")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let can_use_cloud_tunnels = v.get("can_use_cloud_tunnels").and_then(|c| c.as_bool());
    let message = v
        .get("message")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());
    (connected, error, plan, can_use_cloud_tunnels, message)
}

/// POST /v1/register — register a list of ports for the calling process.
#[utoipa::path(
    post,
    path = "/v1/register",
    operation_id = "registerPorts",
    tag = "management",
    summary = "Register (or replace) port→domain mappings for this process",
    description = "Registers one or more `local_port` → `domain` mappings for the calling \
        process. If the process already has registrations they are **replaced** \
        atomically by this call.\n\n\
        The daemon resolves the caller's PID from the TCP source port of this \
        connection, then verifies that the PID is listening on every \
        `local_port` listed. Any port that fails verification causes the entire \
        request to be rejected. Domains must be non-reserved subdomains of \
        `portzero.local`; cloud tunnel domains cannot be registered through this API.",
    request_body(
        content = RegisterRequest,
        example = json!({
            "ports": [
                {"local_port": 8080, "domain": "api.myapp.portzero.local"},
                {"local_port": 9090, "domain": "metrics.myapp.portzero.local"}
            ]
        })
    ),
    responses(
        (status = 200, description = "Mappings registered successfully", body = RegisterResponse,
            example = json!({"pid": 12345, "registered": 2})),
        (status = 422, description = "One or more claimed ports could not be verified", body = ErrorResponse,
            example = json!({"error": "port_not_listening", "detail": "PID 12345 is not listening on port 8080"})),
        (status = 500, description = "Daemon could not identify the calling process", body = ErrorResponse,
            example = json!({"error": "caller_unidentifiable", "detail": "could not resolve source port 54321 to a PID"}))
    )
)]
pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(mut body): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<RegisterResponse>), (StatusCode, Json<ErrorResponse>)> {
    if let Err(detail) = validate_registration_count(body.ports.len()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "invalid_registration_count",
                detail,
            }),
        ));
    }

    for reg in &mut body.ports {
        reg.domain = canonical_registration_domain(&reg.domain).map_err(|detail| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "invalid_domain",
                    detail,
                }),
            )
        })?;
    }

    let pid = resolve_pid(&addr)?;
    for reg in &body.ports {
        if !port_verify::pid_is_listening_on(pid, reg.local_port) {
            tracing::warn!(
                "management: PID {} not listening on port {}",
                pid,
                reg.local_port
            );
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorResponse {
                    error: "port_not_listening",
                    detail: format!("PID {} is not listening on port {}", pid, reg.local_port),
                }),
            ));
        }
    }

    let count = body.ports.len();
    state.store.write().await.insert(pid, body.ports);
    tracing::debug!("management: registered {} port(s) for PID {}", count, pid);

    Ok((
        StatusCode::OK,
        Json(RegisterResponse {
            pid,
            registered: count,
        }),
    ))
}

fn validate_registration_count(count: usize) -> Result<(), String> {
    if (1..=MAX_REGISTRATIONS_PER_PROCESS).contains(&count) {
        Ok(())
    } else {
        Err(format!(
            "Provide between 1 and {MAX_REGISTRATIONS_PER_PROCESS} port registrations"
        ))
    }
}

fn canonical_registration_domain(raw: &str) -> Result<String, String> {
    let domain = raw.trim().to_ascii_lowercase();
    if RESERVED_REGISTRATION_NAMES.contains(&domain.as_str()) {
        return Err(format!("'{domain}' is reserved for portzero management"));
    }
    let Some(prefix) = domain.strip_suffix(".portzero.local") else {
        return Err("domain must be a subdomain of portzero.local".to_string());
    };
    if prefix.is_empty() || domain.len() > 253 {
        return Err(
            "domain must contain a non-empty subdomain and be at most 253 bytes".to_string(),
        );
    }
    for label in prefix.split('.') {
        let valid = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        if !valid {
            return Err(format!("'{domain}' is not a valid DNS name"));
        }
    }
    Ok(domain)
}

/// DELETE /v1/register — remove the registration for the calling process.
#[utoipa::path(
    delete,
    path = "/v1/register",
    operation_id = "deregisterPorts",
    tag = "management",
    summary = "Deregister all port→domain mappings for this process",
    description = "Removes all port→domain mappings previously registered by the calling \
        process. The daemon also performs automatic deregistration when it \
        detects that a PID has exited, so explicit deregistration on graceful \
        shutdown is optional but recommended.",
    responses(
        (status = 200, description = "Mappings deregistered successfully", body = DeregisterResponse,
            example = json!({"pid": 12345, "deregistered": true})),
        (status = 404, description = "No registrations found for this process", body = ErrorResponse,
            example = json!({"error": "not_registered", "detail": "PID 12345 has no active registration"})),
        (status = 500, description = "Daemon could not identify the calling process", body = ErrorResponse,
            example = json!({"error": "caller_unidentifiable", "detail": "could not resolve source port 54321 to a PID"}))
    )
)]
pub async fn deregister(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<(StatusCode, Json<DeregisterResponse>), (StatusCode, Json<ErrorResponse>)> {
    let pid = resolve_pid(&addr)?;

    let removed = state.store.write().await.remove(&pid).is_some();
    if !removed {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not_registered",
                detail: format!("PID {} has no active registration", pid),
            }),
        ));
    }

    tracing::debug!("management: deregistered PID {}", pid);
    Ok((
        StatusCode::OK,
        Json(DeregisterResponse {
            pid,
            deregistered: true,
        }),
    ))
}

/// GET /v1/status — return the current registration for the calling process.
#[utoipa::path(
    get,
    path = "/v1/status",
    operation_id = "getStatus",
    tag = "management",
    summary = "Return current port→domain registrations for this process",
    description = "Returns the port→domain mappings currently registered for the calling \
        process. If the process has no active registrations, `404` is returned.",
    responses(
        (status = 200, description = "Current registrations for this process", body = StatusResponse,
            example = json!({
                "pid": 12345,
                "ports": [
                    {"local_port": 8080, "domain": "api.myapp.portzero.local"},
                    {"local_port": 9090, "domain": "metrics.myapp.portzero.local"}
                ]
            })),
        (status = 404, description = "No registrations found for this process", body = ErrorResponse,
            example = json!({"error": "not_registered", "detail": "PID 12345 has no active registration"})),
        (status = 500, description = "Daemon could not identify the calling process", body = ErrorResponse,
            example = json!({"error": "caller_unidentifiable", "detail": "could not resolve source port 54321 to a PID"}))
    )
)]
pub async fn status(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<(StatusCode, Json<StatusResponse>), (StatusCode, Json<ErrorResponse>)> {
    let pid = resolve_pid(&addr)?;

    let guard = state.store.read().await;
    match guard.get(&pid) {
        Some(ports) => {
            let ports = ports.clone();
            Ok((StatusCode::OK, Json(StatusResponse { pid, ports })))
        }
        None => Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not_registered",
                detail: format!("PID {} has no active registration", pid),
            }),
        )),
    }
}

/// GET /v1/daemon/status — daemon version and coarse health for local apps.
#[utoipa::path(
    get,
    path = "/v1/daemon/status",
    operation_id = "getDaemonStatus",
    tag = "management",
    summary = "Return daemon version and status",
    description = "Returns the running portzero daemon version and coarse status for local \
        applications calling `http://portzero.local`. The `registrations` summary covers \
        local API registrations, which are an **unstable** feature.",
    responses(
        (status = 200, description = "Daemon version and status", body = DaemonStatusResponse,
            example = json!({
                "version": "0.1.0",
                "status": "running",
                "daemon_pid": 12345,
                "overlay_active": true,
                "cloud_connected": false,
                "https": {
                    "enabled": true,
                    "redirect_port_80": false,
                    "passthrough_port_443": true
                },
                "registrations": {
                    "feature_stability": "unstable",
                    "count": 2
                }
            }))
    )
)]
pub async fn daemon_status(State(state): State<AppState>) -> Json<DaemonStatusResponse> {
    let daemon_pid = read_daemon_pid(&state.state_dir);
    let overlay = read_overlay_state(&state.state_dir);
    let (cloud_connected, _, _, _, _) = read_cloud_state(&state.state_dir);
    let https_policy = crate::discovery_loop::DaemonConfig::load().overlay_https;
    let registration_count = state
        .store
        .read()
        .await
        .values()
        .map(Vec::len)
        .sum::<usize>();

    Json(DaemonStatusResponse {
        version: env!("CARGO_PKG_VERSION"),
        status: "running",
        daemon_pid,
        overlay_active: overlay.overlay_active,
        cloud_connected,
        https: HttpsStatus {
            enabled: https_policy.enable_for_port_80,
            redirect_port_80: https_policy.redirect_port_80,
            passthrough_port_443: https_policy.passthrough_port_443,
        },
        registrations: ApiRegistrationsStatus {
            feature_stability: "unstable",
            count: registration_count,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::dashboard::{
        add_duplicate_route_alerts, has_dns_token, read_issues, substitution_alerts,
        tunnel_link_url,
    };
    use super::{canonical_registration_domain, validate_registration_count};
    use crate::net::stack::OverlayHttpsPolicy;
    use std::collections::BTreeMap;

    #[test]
    fn registration_count_must_be_between_one_and_sixty_four() {
        assert!(validate_registration_count(0).is_err());
        assert!(validate_registration_count(1).is_ok());
        assert!(validate_registration_count(64).is_ok());
        assert!(validate_registration_count(65).is_err());
    }

    #[test]
    fn registration_domains_are_canonical_and_management_names_are_reserved() {
        assert_eq!(
            canonical_registration_domain(" API.Dev.PortZero.Local ").unwrap(),
            "api.dev.portzero.local"
        );
        for domain in [
            "portzero.local",
            "api.portzero.local",
            "portzero.portzero.local",
            "portzero-api.portzero.local",
        ] {
            assert!(canonical_registration_domain(domain).is_err(), "{domain}");
        }
    }

    #[test]
    fn registration_domains_reject_non_overlay_invalid_and_oversized_names() {
        let long_label = "a".repeat(64);
        let valid_long_label = "a".repeat(60);
        let long_domain = format!(
            "{}.portzero.local",
            [valid_long_label.as_str(); 4].join(".")
        );
        for domain in [
            "example.com".to_string(),
            "x.local".to_string(),
            ".portzero.local".to_string(),
            "-x.portzero.local".to_string(),
            "x-.portzero.local".to_string(),
            "x_.portzero.local".to_string(),
            format!("{long_label}.portzero.local"),
            long_domain,
        ] {
            assert!(canonical_registration_domain(&domain).is_err(), "{domain}");
        }
    }

    #[test]
    fn tunnel_link_url_uses_scheme_for_local_web_port() {
        use crate::protocol_detect::Canonical;
        let policy = OverlayHttpsPolicy::default();
        assert_eq!(
            tunnel_link_url("web.portzero.local", 80, Some(Canonical::Http), &policy).as_deref(),
            Some("http://web.portzero.local")
        );
        assert_eq!(
            tunnel_link_url(
                "staging.portzero.net.portzero.local",
                443,
                Some(Canonical::Tls),
                &policy
            )
            .as_deref(),
            Some("https://staging.portzero.net.portzero.local")
        );
    }

    #[test]
    fn tunnel_link_url_links_web_backend_on_nonstandard_port() {
        // A dev server on a non-80/443 port is reachable at that exact port on
        // the overlay; link it with the detected scheme.
        use crate::protocol_detect::Canonical;
        let policy = OverlayHttpsPolicy::default();
        assert_eq!(
            tunnel_link_url("web.portzero.local", 3000, Some(Canonical::Http), &policy).as_deref(),
            Some("http://web.portzero.local:3000")
        );
        assert_eq!(
            tunnel_link_url("app.portzero.local", 8443, Some(Canonical::Tls), &policy).as_deref(),
            Some("https://app.portzero.local:8443")
        );
    }

    #[test]
    fn tunnel_link_url_skips_non_web_local_ports() {
        // A raw TCP service (no detected web protocol) on a non-80/443 port has
        // no browser URL.
        let policy = OverlayHttpsPolicy::default();
        assert_eq!(
            tunnel_link_url("db.portzero.local", 5432, None, &policy),
            None
        );
        assert_eq!(
            tunnel_link_url("admin.portzero.local", 8080, None, &policy),
            None
        );
    }

    #[test]
    fn tunnel_link_url_never_links_reserved_management_name() {
        // `api.portzero.local` / `portzero.local` are the daemon's own
        // management server, never a customer tunnel — never clickable.
        use crate::protocol_detect::Canonical;
        let policy = OverlayHttpsPolicy::default();
        assert_eq!(
            tunnel_link_url("api.portzero.local", 443, Some(Canonical::Tls), &policy),
            None
        );
        assert_eq!(
            tunnel_link_url("portzero.local", 80, Some(Canonical::Http), &policy),
            None
        );
    }

    #[test]
    fn tunnel_link_url_port_80_is_https_when_local_https_enabled() {
        // With HTTPS-for-port-80 on, a port-80 tunnel is served over HTTPS (and
        // plain HTTP is redirected there), so the clickable link is https://.
        let policy = OverlayHttpsPolicy {
            enable_for_port_80: true,
            ..OverlayHttpsPolicy::default()
        };
        assert_eq!(
            tunnel_link_url("web.portzero.local", 80, None, &policy).as_deref(),
            Some("https://web.portzero.local")
        );
    }

    #[test]
    fn tunnel_link_url_cloud_is_always_https_regardless_of_backend_port() {
        // Cloud tunnels are served over HTTPS by the edge, which terminates TLS
        // regardless of the local backend port the process listens on.
        let policy = OverlayHttpsPolicy::default();
        assert_eq!(
            tunnel_link_url("api--alice.tunnel.portzero.cloud", 8080, None, &policy).as_deref(),
            Some("https://api--alice.tunnel.portzero.cloud")
        );
        assert_eq!(
            tunnel_link_url("web--bob.tunnel.portzero.cloud", 3000, None, &policy).as_deref(),
            Some("https://web--bob.tunnel.portzero.cloud")
        );
    }

    #[test]
    fn status_json_surfaces_issues_from_issues_json() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::notify::IssuesState {
            issues: vec![crate::notify::Issue::InvalidCloudTunnelScope {
                domain: "myservice.portzero.cloud".to_string(),
                reason: "missing username scope".to_string(),
                context: "pid 42".to_string(),
                pid: Some(42),
            }],
        };
        crate::notify::write_issues(&dir.path().join("issues.json"), &state);

        let loaded = read_issues(dir.path());
        assert_eq!(loaded.issues.len(), 1);
        assert!(loaded.issues[0]
            .summary()
            .contains("myservice.portzero.cloud"));
    }

    #[test]
    fn dns_token_matching_requires_boundaries() {
        assert!(has_dns_token("web-main.portzero.local", "main"));
        assert!(has_dns_token("web.feature-x.portzero.local", "feature-x"));
        assert!(!has_dns_token("web-maintenance.portzero.local", "main"));
    }

    #[test]
    fn substitution_alert_suggests_branch_placeholder_for_literal_value() {
        let substitutions = BTreeMap::from([
            ("branch".to_string(), "main".to_string()),
            ("worktree".to_string(), "portzero-local".to_string()),
        ]);

        let alerts = substitution_alerts("web-main.portzero.local", &substitutions);

        assert_eq!(alerts.len(), 1);
        assert_eq!(
            alerts[0].get("title").and_then(|v| v.as_str()),
            Some("Suggest replacing \"main\" with \"{branch}\"")
        );
        assert_eq!(
            alerts[0].get("severity").and_then(|v| v.as_str()),
            Some("info")
        );
        assert!(alerts[0]
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("PZ_TUNNEL supports variable substitution"));
    }

    #[test]
    fn substitution_alert_skips_existing_placeholder() {
        let substitutions = BTreeMap::from([("branch".to_string(), "main".to_string())]);

        let alerts = substitution_alerts("web-{branch}.portzero.local", &substitutions);

        assert!(alerts.is_empty());
    }

    #[test]
    fn substitution_alert_warns_when_branch_placeholder_resolves_to_unknown() {
        let substitutions = BTreeMap::from([("branch".to_string(), "unknown".to_string())]);

        let alerts = substitution_alerts("web-{branch}.portzero.local", &substitutions);

        assert_eq!(alerts.len(), 1);
        assert_eq!(
            alerts[0].get("title").and_then(|v| v.as_str()),
            Some("Could not determine {branch}")
        );
        assert_eq!(
            alerts[0].get("severity").and_then(|v| v.as_str()),
            Some("info")
        );
        assert!(alerts[0]
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("materialized the value as \"unknown\""));
    }

    #[test]
    fn duplicate_route_alerts_mark_every_matching_domain() {
        let mut local_services = vec![
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
        ];
        let mut cloud_routes = vec![serde_json::json!({
            "domain": "api.portzero.local",
            "port": 3000,
            "alerts": [],
        })];

        add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

        for row in local_services.iter().chain(cloud_routes.iter()) {
            let alerts = row.get("alerts").and_then(|v| v.as_array()).unwrap();
            assert_eq!(alerts.len(), 1);
            assert_eq!(
                alerts[0].get("title").and_then(|v| v.as_str()),
                Some("Duplicate tunnel domain")
            );
            assert_eq!(
                alerts[0].get("severity").and_then(|v| v.as_str()),
                Some("warning")
            );
            assert!(alerts[0]
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap()
                .contains("3 tunnels materialized to api.portzero.local"));
        }
    }

    #[test]
    fn duplicate_route_alerts_include_different_ports() {
        let mut local_services = vec![
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3001,
                "alerts": [],
            }),
        ];
        let mut cloud_routes = Vec::new();

        add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

        assert!(local_services
            .iter()
            .all(|row| { row.get("alerts").and_then(|v| v.as_array()).unwrap().len() == 1 }));
    }

    #[test]
    fn duplicate_route_alerts_skip_unique_domains() {
        let mut local_services = vec![
            serde_json::json!({
                "domain": "api.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
            serde_json::json!({
                "domain": "web.portzero.local",
                "service_port": 3000,
                "alerts": [],
            }),
        ];
        let mut cloud_routes = Vec::new();

        add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

        assert!(local_services.iter().all(|row| row
            .get("alerts")
            .and_then(|v| v.as_array())
            .unwrap()
            .is_empty()));
    }
}
