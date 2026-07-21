//! The developer status dashboard: the self-contained HTML shell served at
//! `portzero.local`, the `/status.json` snapshot it renders from, and the
//! branding assets and OpenAPI document it links to.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use axum::Json;

use super::{read_cloud_state, read_daemon_pid, read_overlay_state};
use crate::management::server::{AppState, PortRegistration};
use crate::route_table::RouteTable;

const PORTZERO_MARK_JPEG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/management/assets/portzero-mark.jpg"
));
const PORTZERO_WORDMARK_JPEG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/management/assets/portzero-wordmark.jpg"
));
const GETTING_STARTED_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../installer/getting-started.json"
));

/// Read and parse `routes.json`.  Returns an empty table on any error.
fn read_route_table(state_dir: &std::path::Path) -> RouteTable {
    RouteTable::load(&state_dir.join("routes.json")).unwrap_or_default()
}

/// Read `cloud_route_status.json` (domain → review status). Empty on any error.
fn read_cloud_route_statuses(
    state_dir: &std::path::Path,
) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(state_dir.join("cloud_route_status.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Read and parse `issues.json` (visibility-layer problems: duplicate overlay
/// names, legacy listeners, Docker port conflicts, invalidly-scoped cloud
/// tunnel domains). Returns an empty state on any error, matching `portzero
/// status`'s CLI behavior for the same file.
pub(super) fn read_issues(state_dir: &std::path::Path) -> crate::notify::IssuesState {
    crate::notify::read_issues(&state_dir.join("issues.json"))
}

fn command_on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            dir.join(format!("{program}.exe")).is_file()
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

fn detected_languages() -> serde_json::Value {
    let languages = [
        ("typescript", "TypeScript", &["node", "nodejs", "npm"][..]),
        ("javascript", "JavaScript", &["node", "nodejs"][..]),
        ("python", "Python", &["python3", "python"][..]),
        ("java", "Java", &["java", "javac"][..]),
        ("go", "Go", &["go"][..]),
        ("rust", "Rust", &["cargo", "rustc"][..]),
        ("csharp", "C#", &["dotnet"][..]),
        ("php", "PHP", &["php", "composer"][..]),
        ("ruby", "Ruby", &["ruby", "bundle"][..]),
        (
            "cpp",
            "C/C++",
            &["cc", "gcc", "clang", "g++", "clang++"][..],
        ),
        ("swift", "Swift", &["swift"][..]),
        ("kotlin", "Kotlin", &["kotlin", "kotlinc"][..]),
        ("dart", "Dart", &["dart"][..]),
        ("elixir", "Elixir", &["elixir", "mix"][..]),
        ("scala", "Scala", &["scala", "sbt"][..]),
        ("r", "R", &["R", "Rscript"][..]),
        ("julia", "Julia", &["julia"][..]),
        ("lua", "Lua", &["lua", "luajit"][..]),
        ("perl", "Perl", &["perl"][..]),
        ("zig", "Zig", &["zig"][..]),
        ("haskell", "Haskell", &["ghc", "cabal", "stack"][..]),
        ("shell", "Shell", &["bash", "sh"][..]),
    ];

    let detected: Vec<serde_json::Value> = languages
        .iter()
        .map(|(id, label, tools)| {
            let installed_tools: Vec<&str> = tools
                .iter()
                .copied()
                .filter(|tool| command_on_path(tool))
                .collect();
            serde_json::json!({
                "id": id,
                "label": label,
                "installed": !installed_tools.is_empty(),
                "tools": installed_tools,
            })
        })
        .collect();

    let default_language = detected
        .iter()
        .find(|lang| {
            lang.get("id").and_then(|v| v.as_str()) != Some("shell")
                && lang
                    .get("installed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
        .and_then(|lang| lang.get("id").and_then(|v| v.as_str()))
        .unwrap_or("typescript");

    serde_json::json!({
        "default": default_language,
        "items": detected,
    })
}

fn detected_environment() -> serde_json::Value {
    let (os_id, os_label) = if cfg!(target_os = "macos") {
        ("macos", "macOS")
    } else if cfg!(target_os = "windows") {
        ("windows", "Windows")
    } else if cfg!(target_os = "linux") {
        ("linux", "Linux")
    } else {
        ("unix", std::env::consts::OS)
    };

    serde_json::json!({
        "os": {
            "id": os_id,
            "label": os_label,
        },
        "docker": {
            "installed": command_on_path("docker"),
        },
    })
}

fn getting_started_manifest() -> serde_json::Value {
    serde_json::from_str(GETTING_STARTED_JSON).unwrap_or_else(|_| {
        serde_json::json!({
            "examples": [],
            "languages": [],
        })
    })
}

fn display_substitutions(
    substitutions: &std::collections::BTreeMap<String, String>,
    source: &crate::discovery::ServiceSource,
) -> serde_json::Value {
    let mut values = substitutions.clone();
    if !values.contains_key("folder-name") {
        let folder_name = match source {
            crate::discovery::ServiceSource::Process { cwd } => cwd
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            crate::discovery::ServiceSource::Container { name, .. } => name.clone(),
        };
        values.insert("folder-name".to_string(), folder_name);
    }
    serde_json::json!(values)
}

/// Resolve the browser-openable URL for a discovered tunnel, or `None` when it
/// is not an HTTP/HTTPS endpoint a browser can open. Drives the clickable
/// domain links in both the built-in dashboard and the desktop app.
///
/// - **Cloud tunnels** (any domain that is not a `.portzero.local` overlay
///   name) are served over HTTPS at their public domain by the edge, which
///   terminates TLS regardless of the local backend port — so they are always
///   reachable, and clickable, at `https://{domain}`.
/// - **Local overlay tunnels** are reached by name on the virtual IP: port 443
///   is HTTPS and port 80 is HTTP. When the HTTPS-for-port-80 policy is on, a
///   port-80 tunnel is also served over HTTPS on 443 (with plain HTTP redirected
///   there), so its canonical link is `https://` even though it asked to be
///   exposed on 80. Any other port is not a web port and yields `None`.
pub(super) fn tunnel_link_url(
    domain: &str,
    service_port: u16,
    https_policy: &crate::net::stack::OverlayHttpsPolicy,
) -> Option<String> {
    if !crate::discovery::is_local_overlay_domain(domain) {
        return Some(format!("https://{domain}"));
    }
    match service_port {
        443 => Some(format!("https://{domain}")),
        80 if https_policy.enable_for_port_80 => Some(format!("https://{domain}")),
        80 => Some(format!("http://{domain}")),
        _ => None,
    }
}

pub(super) fn has_dns_token(template: &str, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }

    template.match_indices(value).any(|(start, _)| {
        let end = start + value.len();
        let before_is_boundary = template[..start]
            .chars()
            .next_back()
            .map(|c| !c.is_ascii_alphanumeric())
            .unwrap_or(true);
        let after_is_boundary = template[end..]
            .chars()
            .next()
            .map(|c| !c.is_ascii_alphanumeric())
            .unwrap_or(true);
        before_is_boundary && after_is_boundary
    })
}

pub(super) fn substitution_alerts(
    domain_template: &str,
    substitutions: &std::collections::BTreeMap<String, String>,
) -> Vec<serde_json::Value> {
    const SUGGESTED_KEYS: &[&str] = &[
        "branch",
        "worktree",
        "project",
        "user",
        "machine",
        "uid",
        "local-username",
        "cloud-username",
        "folder-name",
    ];

    let mut alerts = Vec::new();

    if domain_template.contains("{branch}")
        && substitutions.get("branch").map(String::as_str) == Some("unknown")
    {
        alerts.push(serde_json::json!({
            "severity": "info",
            "title": "Could not determine {branch}",
            "detail": "PZ_TUNNEL uses {branch}, but the daemon could not determine the current git branch, so it materialized the value as \"unknown\". Run the service from a git worktree with a checked-out branch, or use a literal tunnel name."
        }));
    }

    alerts.extend(SUGGESTED_KEYS
        .iter()
        .filter_map(|key| {
            let placeholder = format!("{{{key}}}");
            if domain_template.contains(&placeholder) {
                return None;
            }

            let value = substitutions.get(*key)?;
            if !has_dns_token(domain_template, value) {
                return None;
            }

            Some(serde_json::json!({
                "severity": "info",
                "title": format!("Suggest replacing \"{value}\" with \"{placeholder}\""),
                "detail": format!(
                    "PZ_TUNNEL supports variable substitution: write {placeholder} in the value and the daemon materializes it as \"{value}\" for this process. This keeps tunnel names accurate when the branch, worktree, user, or machine changes."
                ),
            }))
        }));

    alerts
}

fn duplicate_route_alert(domain: &str, duplicate_count: usize) -> serde_json::Value {
    serde_json::json!({
        "severity": "warning",
        "title": "Duplicate tunnel domain",
        "detail": format!(
            "{duplicate_count} tunnels materialized to {domain}. Change PZ_TUNNEL so each active tunnel has a unique domain."
        ),
    })
}

fn append_alert(row: &mut serde_json::Value, alert: serde_json::Value) {
    row.get_mut("alerts")
        .and_then(|alerts| alerts.as_array_mut())
        .expect("route rows always include an alerts array")
        .push(alert);
}

pub(super) fn add_duplicate_route_alerts(
    local_services: &mut [serde_json::Value],
    cloud_routes: &mut [serde_json::Value],
) {
    #[derive(Clone, Copy)]
    enum RouteList {
        Local,
        Cloud,
    }

    let mut routes = Vec::new();
    for (index, row) in local_services.iter().enumerate() {
        if let Some(domain) = row.get("domain").and_then(|v| v.as_str()) {
            routes.push((RouteList::Local, index, domain.to_string()));
        }
    }
    for (index, row) in cloud_routes.iter().enumerate() {
        if let Some(domain) = row.get("domain").and_then(|v| v.as_str()) {
            routes.push((RouteList::Cloud, index, domain.to_string()));
        }
    }

    let mut counts = std::collections::HashMap::new();
    for (_, _, domain) in &routes {
        *counts.entry(domain.clone()).or_insert(0usize) += 1;
    }

    for (list, index, domain) in routes {
        let duplicate_count = counts.get(&domain).copied().unwrap_or(0);
        if duplicate_count < 2 {
            continue;
        }

        let alert = duplicate_route_alert(&domain, duplicate_count);
        match list {
            RouteList::Local => append_alert(&mut local_services[index], alert),
            RouteList::Cloud => append_alert(&mut cloud_routes[index], alert),
        }
    }
}

fn management_registrations_json(
    registrations: &std::collections::HashMap<u32, Vec<PortRegistration>>,
) -> Vec<serde_json::Value> {
    registrations
        .iter()
        .flat_map(|(pid, regs)| {
            regs.iter().map(move |r| {
                serde_json::json!({
                    "pid": pid,
                    "local_port": r.local_port,
                    "domain": r.domain,
                    "feature_stability": "unstable",
                })
            })
        })
        .collect()
}

/// GET / — serve the developer status dashboard as a self-contained HTML shell.
/// The page fetches `/status.json` and renders the UI client-side, refreshing every 3 s.
pub async fn status_ui() -> Html<&'static str> {
    Html(STATUS_UI_HTML)
}

/// GET /login — start browser login and redirect this tab to the cloud auth UI.
pub async fn start_login() -> impl IntoResponse {
    match crate::auth::start_browser_login_session().await {
        Ok(session) => {
            tracing::info!(
                callback_port = session.callback_port,
                "started dashboard browser login"
            );
            Redirect::temporary(&session.auth_url).into_response()
        }
        Err(error) => {
            tracing::warn!(?error, "failed to start dashboard browser login");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to start login flow: {error}"),
            )
                .into_response()
        }
    }
}

/// GET /assets/portzero-mark.jpg — favicon and compact brand mark.
pub async fn portzero_mark_asset() -> impl IntoResponse {
    ([("content-type", "image/jpeg")], PORTZERO_MARK_JPEG)
}

/// GET /assets/portzero-wordmark.jpg — full PortZero wordmark.
pub async fn portzero_wordmark_asset() -> impl IntoResponse {
    ([("content-type", "image/jpeg")], PORTZERO_WORDMARK_JPEG)
}

/// GET /openapi.json — read-only OpenAPI document for local process clients.
pub async fn openapi_json() -> impl IntoResponse {
    use utoipa::OpenApi;
    let spec = crate::management::openapi::ManagementApiDoc::openapi()
        .to_pretty_json()
        .unwrap_or_default();
    ([("content-type", "application/json")], spec)
}

const STATUS_UI_HTML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/management/assets/dashboard.html"
));

/// GET /status.json — machine-readable snapshot of all daemon state.
pub async fn status_json(State(state): State<AppState>) -> Json<serde_json::Value> {
    let daemon_pid = read_daemon_pid(&state.state_dir);
    let overlay = read_overlay_state(&state.state_dir);
    let routes = read_route_table(&state.state_dir);
    let (cloud_connected, cloud_error, cloud_plan, cloud_can_use_tunnels, cloud_message) =
        read_cloud_state(&state.state_dir);
    let auth_authenticated = crate::auth::AuthConfig::load().is_authenticated();
    let diagnostics = crate::diagnostics::load_report(&state.state_dir);
    let issues = read_issues(&state.state_dir);

    let registrations_guard = state.store.read().await;
    let management_registrations = management_registrations_json(&registrations_guard);

    let https_policy = crate::discovery_loop::DaemonConfig::load().overlay_https;

    let mut local_services: Vec<serde_json::Value> = overlay
        .routes
        .iter()
        .map(|r| {
            let domain_template = if r.domain_template.is_empty() {
                &r.domain
            } else {
                &r.domain_template
            };
            serde_json::json!({
                "domain": r.domain,
                "domain_template": domain_template,
                "substitutions": display_substitutions(&r.substitutions, &r.source),
                "alerts": substitution_alerts(domain_template, &r.substitutions),
                "real_addr": r.real_addr,
                "service_port": r.service_port,
                "health_path": r.health_path,
                "link_url": tunnel_link_url(&r.domain, r.service_port, &https_policy),
                "pid": r.pid,
            })
        })
        .collect();

    let cloud_route_statuses = read_cloud_route_statuses(&state.state_dir);
    let mut cloud_routes: Vec<serde_json::Value> = routes
        .routes
        .values()
        .map(|r| {
            let domain_template = if r.domain_template.is_empty() {
                &r.domain
            } else {
                &r.domain_template
            };
            // Review status ("pending_review"/"published"/"denied"); default to
            // "published" for local overlay routes that never carry a status.
            let status = cloud_route_statuses
                .get(&r.domain)
                .map(String::as_str)
                .unwrap_or("published");
            serde_json::json!({
                "domain": r.domain,
                "domain_template": domain_template,
                "substitutions": display_substitutions(&r.substitutions, &r.source),
                "alerts": substitution_alerts(domain_template, &r.substitutions),
                "port": r.port,
                "health_path": r.health_path,
                "link_url": tunnel_link_url(&r.domain, r.port, &https_policy),
                "pid": r.pid,
                "status": status,
            })
        })
        .collect();

    add_duplicate_route_alerts(&mut local_services, &mut cloud_routes);

    // A single ranked list of problems, combining issues.json (passively
    // detected duplicate names / port conflicts / misconfigured tunnels) and
    // diagnostics.json (the on-demand Binary/Network/DNS/TLS/Auth/System
    // health-check report) — the same list the tray renders from, so
    // "issues" and "diagnostics" don't read as two different things.
    let problems_json: Vec<serde_json::Value> =
        crate::notify::collect_problems(&issues, diagnostics.as_ref())
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null))
            .collect();
    let diagnostics_checks_run = diagnostics.as_ref().map(|r| r.checks_run);

    Json(serde_json::json!({
        "daemon_pid": daemon_pid,
        "overlay_active": overlay.overlay_active,
        "auth_authenticated": auth_authenticated,
        "local_services": local_services,
        "cloud_connected": cloud_connected,
        "cloud_error": cloud_error,
        "cloud_plan": cloud_plan,
        "cloud_can_use_tunnels": cloud_can_use_tunnels,
        "cloud_message": cloud_message,
        "cloud_routes": cloud_routes,
        "management_registrations": management_registrations,
        "management_registrations_feature_stability": "unstable",
        "problems": problems_json,
        "diagnostics_checks_run": diagnostics_checks_run,
        "languages": detected_languages(),
        "environment": detected_environment(),
        "getting_started": getting_started_manifest(),
        "examples": super::examples::status_value(&state).await,
        "auto_open_http_tunnels": crate::discovery_loop::DaemonConfig::load().auto_open_http_tunnels,
        "https_policy": {
            "enable_for_port_80": https_policy.enable_for_port_80,
            "redirect_port_80": https_policy.redirect_port_80,
            "passthrough_port_443": https_policy.passthrough_port_443,
        },
    }))
}
