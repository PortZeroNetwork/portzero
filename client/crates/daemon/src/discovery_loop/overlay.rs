//! `.portzero.local` overlay service refresh and the daemon-wide issue
//! gathering/publishing that surfaces problems in `portzero status`.

use std::sync::Arc;

use tokio::sync::Notify;

use crate::discovery::{self, DiscoveredNetworkService};
use crate::net::overlay::OverlayNetwork;
use crate::net::service_table::ServiceTable;
use crate::notify::{self, IssuesState};
use crate::route_table::{OverlayRoute, OverlayState, RouteTable};

use super::config::DaemonConfig;

/// Persist discovered overlay services to `overlay.json` so `portzero status` can read them.
pub(super) fn write_overlay_state(
    config: &DaemonConfig,
    services: &[DiscoveredNetworkService],
    overlay_active: bool,
) {
    let routes = services
        .iter()
        .map(|s| OverlayRoute {
            domain: format!("{}.portzero.local", s.name),
            domain_template: s.domain_template.clone(),
            substitutions: s.substitutions.clone(),
            service_port: s.service_port,
            real_addr: s.real_addr.to_string(),
            health_path: s.health_path.clone(),
            pid: s.pid,
            source: s.source.clone(),
        })
        .collect();
    let state = OverlayState {
        overlay_active,
        routes,
    };
    if let Err(e) = state.save(&config.overlay_path()) {
        tracing::warn!("Failed to save overlay state: {}", e);
    }
}

/// Build an overlay `ServiceTable` from the services discovered for the local
/// virtual network (those whose `PZ_TUNNEL` value ends in `.portzero.local`).
///
/// When `mgmt_port != 0` the management service ("portzero") is injected as the
/// first entry, mapping to `127.0.0.1:<mgmt_port>` on virtual port 80.
///
/// This is pure (no TUN / no privileges required) so it can be unit-tested.
pub(super) fn build_overlay_table(
    services: &[DiscoveredNetworkService],
    mgmt_port: u16,
) -> ServiceTable {
    let mut table = ServiceTable::new();
    if mgmt_port != 0 {
        let backend = std::net::SocketAddr::from(([127, 0, 0, 1], mgmt_port));
        table.register("portzero".to_string(), backend, 80, 0);
        table.register("portzero-api".to_string(), backend, 80, 0);
    }
    for svc in services {
        table.register_with_backend_protocol(
            svc.name.clone(),
            svc.real_addr,
            svc.service_port,
            svc.pid,
            svc.backend_protocol,
        );
    }
    table
}

/// Best-effort: push the latest set of `.portzero.local` overlay services into the
/// running overlay network. Logs and ignores errors so a transient stack issue
/// never disrupts the (independent) cloud/local route flow.
///
/// Returns the duplicate-name visibility issues detected over the freshly
/// scanned services *and* the scanned overlay service list itself (so the caller
/// can build a managed-context for legacy-listener monitoring without scanning
/// twice). The caller merges these issues with other sources and publishes them
/// once per scan.
pub(super) async fn refresh_overlay_services(
    overlay: &OverlayNetwork,
    mgmt_port: u16,
    mgmt_store: &crate::management::RegistrationStore,
) -> (Vec<notify::Issue>, Vec<DiscoveredNetworkService>) {
    let services = discovery::scan_network_services(mgmt_store).await;
    let issues = notify::detect_duplicate_names(&services);
    let table = build_overlay_table(&services, mgmt_port);
    if let Err(e) = overlay.update_services(table).await {
        tracing::warn!("Failed to update overlay services: {:#}", e);
    }
    (issues, services)
}

/// Low-latency path for first DNS hits on unknown `*.portzero.local` names.
///
/// The main discovery loop performs full reconciliation, including Docker scans,
/// cloud sync, diagnostics, and issue publishing. That is correct but too much
/// work to put on the critical path of a browser's first DNS lookup. This task
/// reacts to DNS misses independently and refreshes only local process overlay
/// routes, which is the common `PZ_TUNNEL=foo.portzero.local cargo run` case.
pub(super) async fn run_dns_fast_overlay_refresh(
    config: DaemonConfig,
    overlay: Arc<OverlayNetwork>,
    mgmt_port: u16,
    mgmt_store: crate::management::RegistrationStore,
    dns_rescan: Arc<Notify>,
) {
    loop {
        dns_rescan.notified().await;

        let Some(services) = discovery::scan_network_process_services(&mgmt_store).await else {
            continue;
        };
        let table = build_overlay_table(&services, mgmt_port);
        if let Err(e) = overlay.update_services(table).await {
            tracing::warn!("Fast overlay refresh failed to update services: {e:#}");
            continue;
        }
        write_overlay_state(&config, &services, true);
        tracing::trace!(
            "Fast overlay refresh complete: {} process service(s)",
            services.len()
        );
    }
}

/// Gather all current issues from every source and publish them once.
///
/// Sources:
///  - duplicate `.portzero.local` names (from the overlay scan, when the overlay
///    is running),
///  - legacy listeners: processes serving common/managed ports directly,
///    bypassing port-zero.
///
/// `overlay_issues` / `overlay_services` come from a prior overlay refresh (or
/// are empty when the overlay isn't running). Best-effort and non-fatal.
#[allow(clippy::too_many_arguments)]
pub(super) async fn gather_and_publish_issues(
    config: &DaemonConfig,
    route_table: &RouteTable,
    overlay_issues: Vec<notify::Issue>,
    overlay_services: &[DiscoveredNetworkService],
    docker_conflicts: Vec<notify::Issue>,
    cloud_scope_issues: Vec<notify::Issue>,
    notified_issues: &mut IssuesState,
    scan_legacy: bool,
    legacy_cache: &mut Vec<notify::Issue>,
) {
    // The legacy-listener sweep enumerates EVERY process's listening ports —
    // a full-system scan that can take tens of seconds on busy machines even
    // after the per-OS enumeration was de-subprocessed. Running it every 2s
    // iteration wedged the loop (routes/overlay adoption starved behind it in
    // CI); it is advisory, so run it on the same ~30s cadence as diagnostics
    // and republish the cached result in between.
    if scan_legacy {
        let managed = build_managed_context(route_table, overlay_services);
        // Blocking full-system scan: run it on the spawn_blocking pool so the
        // async executor stays free for shutdown signals and other tasks.
        *legacy_cache = tokio::task::spawn_blocking(move || {
            crate::legacy_monitor::scan_legacy_listeners(&managed)
        })
        .await
        .unwrap_or_default();
    }
    let mut all = overlay_issues;
    all.extend(legacy_cache.iter().cloned());
    // Real-time Docker port-bind conflicts caught by the event monitor (task-8).
    all.extend(docker_conflicts);
    // Cloud-side issues found while scanning routes this pass: invalidly-scoped
    // PZ_TUNNEL cloud tunnel domains (e.g. myservice.portzero.cloud instead of
    // myservice--<cloud-username>.tunnel.portzero.cloud) and duplicate cloud
    // tunnel URLs claimed by more than one context.
    all.extend(cloud_scope_issues);
    publish_issues(all, config, notified_issues);
}

/// Build the set of ports/cwds the daemon already manages (route table routes
/// plus overlay services), so the legacy-listener monitor never flags a service
/// we are already tunneling. Pure given its inputs.
fn build_managed_context(
    route_table: &RouteTable,
    overlay_services: &[DiscoveredNetworkService],
) -> crate::legacy_monitor::ManagedContext {
    use crate::discovery::ServiceSource;
    let mut managed = crate::legacy_monitor::ManagedContext::new();
    for route in route_table.routes.values() {
        if route.port != 0 {
            managed.ports.insert(route.port);
        }
        if let ServiceSource::Process { cwd: Some(cwd) } = &route.source {
            managed.cwds.insert(cwd.clone());
        }
    }
    for svc in overlay_services {
        managed.ports.insert(svc.real_addr.port());
        if let ServiceSource::Process { cwd: Some(cwd) } = &svc.source {
            managed.cwds.insert(cwd.clone());
        }
    }
    managed
}

/// Persist the full set of current issues (from all sources) so
/// `portzero status` can surface them, and — only when the set changes
/// since the last scan — log actionable guidance and fire a single native
/// notification. The change-gating prevents flooding the log/desktop every scan.
///
/// `Issue::LegacyListener` is handled separately from every other issue kind:
/// it is extremely common (any process on a common dev port that hasn't set
/// `PZ_TUNNEL`, including unrelated system services) and not something most
/// users can act on, so it is only ever logged at `info` level — never
/// surfaced as a `portzero status`/tray/desktop-app problem and never fires a
/// desktop notification. See `notify::collect_problems`, which filters it out
/// of the user-facing problem list.
pub(super) fn publish_issues(
    issues: Vec<notify::Issue>,
    config: &DaemonConfig,
    notified_issues: &mut IssuesState,
) {
    let current = IssuesState {
        issues: issues.clone(),
    };

    // Persist the current state so `portzero status` can surface it.
    notify::write_issues(&config.issues_path(), &current);

    if current == *notified_issues {
        return;
    }

    let (legacy, notifiable): (Vec<_>, Vec<_>) = issues
        .iter()
        .cloned()
        .partition(|issue| matches!(issue, notify::Issue::LegacyListener { .. }));

    for issue in &legacy {
        tracing::info!("{} — {}", issue.summary(), issue.fix_hint());
    }

    let had_notifiable = notified_issues
        .issues
        .iter()
        .any(|issue| !matches!(issue, notify::Issue::LegacyListener { .. }));

    if notifiable.is_empty() {
        if had_notifiable {
            tracing::info!("All previously reported tunnel issues are now resolved");
        }
    } else {
        for issue in &notifiable {
            tracing::warn!("{} — {}", issue.summary(), issue.fix_hint());
        }
        // One consolidated notification covering all current notifiable issues.
        let first = &notifiable[0];
        let title = if notifiable.len() == 1 {
            "portzero: issue detected".to_string()
        } else {
            format!("portzero: {} issues", notifiable.len())
        };
        let body = format!("{}\n{}", first.summary(), first.fix_hint());
        notify::send_notification(&title, &body);
    }

    *notified_issues = current;
}

/// Fire a single desktop notification when the set of post-setup regressions
/// changes to a non-empty set, pointing the user at the fix.
///
/// A "post-setup regression" is a step `portzero setup` / the installer
/// configured that has since been removed or broken out-of-band (CA trust
/// removed, Linux capabilities dropped, the dashboard hosts pin deleted, the
/// autostart service removed, …). These are already detected by the diagnostics
/// pass and surfaced in `portzero status`; this turns the setup-related ones
/// into a loud, actionable alert.
///
/// De-duplicated exactly like the resolver self-heal: we re-notify only when the
/// set of problems *changes* (mirroring `publish_issues`), and reset silently
/// once everything setup-related is healthy again — so a still-present, unchanged
/// problem does not nag every 30s. Best-effort and non-fatal.
pub(super) fn notify_post_setup_regressions(
    diag: &crate::diagnostics::DiagnosticsReport,
    last_notified: &mut Vec<String>,
) {
    let mut regressions: Vec<&crate::diagnostics::Diagnostic> = diag
        .issues
        .iter()
        .filter(|d| crate::diagnostics::is_post_setup_regression(&d.id))
        .collect();
    // Stable, severity-then-id ordering so the "first" issue we headline and the
    // change-detection key are both deterministic across scans.
    regressions.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.id.cmp(&b.id)));
    let current_ids: Vec<String> = regressions.iter().map(|d| d.id.clone()).collect();

    if current_ids == *last_notified {
        return;
    }

    if let Some(first) = regressions.first() {
        for d in &regressions {
            tracing::warn!("post-setup regression: {} ({})", d.title, d.id);
        }
        let title = if regressions.len() == 1 {
            "portzero: setup problem detected".to_string()
        } else {
            format!("portzero: {} setup problems", regressions.len())
        };
        // Prefer the diagnostic's concrete fix command, falling back to its
        // description, and finally to the generic setup command.
        let fix = first
            .fix
            .as_ref()
            .map(|f| f.command.clone().unwrap_or_else(|| f.description.clone()))
            .unwrap_or_else(|| "Run `sudo portzero setup` to fix it.".to_string());
        let body = format!("{}\n{}", first.title, fix);
        notify::send_notification(&title, &body);
    } else {
        tracing::info!("All previously reported post-setup regressions are now resolved");
    }

    *last_notified = current_ids;
}
