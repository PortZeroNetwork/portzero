//! Discovery daemon loop: periodically scan for services and update routes.
//!
//! The daemon runs as a background process, scanning every few seconds for
//! processes and Docker containers with PZ_TUNNEL set. Changes are
//! persisted to `~/.portzero/daemon/routes.json`.
//!
//! When authenticated, the daemon also connects to the cloud edge and
//! registers/unregisters routes as they are discovered or removed.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Notify;

const RECONNECT_BASE_DELAY_SECS: u64 = 1;
const RECONNECT_MAX_DELAY_SECS: u64 = 300;
/// How often the daemon re-reads auth.json to pick up login/logout changes.
const AUTH_CHECK_INTERVAL_SECS: u64 = 30;
/// Refresh the JWT when it has less than 1 hour remaining.
const TOKEN_REFRESH_THRESHOLD_SECS: u64 = 3600;
/// How often to check if the token needs refreshing.
const TOKEN_REFRESH_CHECK_INTERVAL_SECS: u64 = 300;
/// How often to poll ~/.portzero/config.toml for external changes (or writes
/// performed via the local management API) and hot-reload the effective values.
/// A short interval makes `scan_interval`, overlay https policy, and dns
/// first-hit policy updates visible quickly without requiring a daemon restart.
const CONFIG_RELOAD_INTERVAL_SECS: u64 = 5;
const OVERLAY_REFRESH_TIMEOUT: Duration = Duration::from_secs(3);

struct ReconnectBackoff {
    next_attempt_at: Option<Instant>,
    current_delay: Duration,
}

impl ReconnectBackoff {
    fn new() -> Self {
        Self {
            next_attempt_at: None,
            current_delay: Duration::from_secs(RECONNECT_BASE_DELAY_SECS),
        }
    }

    fn is_due(&self) -> bool {
        self.next_attempt_at.is_none_or(|t| Instant::now() >= t)
    }

    fn on_failure(&mut self) {
        tracing::info!(
            "Will retry cloud reconnect in {}s",
            self.current_delay.as_secs()
        );
        self.next_attempt_at = Some(Instant::now() + self.current_delay);
        self.current_delay =
            (self.current_delay * 2).min(Duration::from_secs(RECONNECT_MAX_DELAY_SECS));
    }

    fn on_success(&mut self) {
        self.next_attempt_at = None;
        self.current_delay = Duration::from_secs(RECONNECT_BASE_DELAY_SECS);
    }
}

use anyhow::{Context, Result};
use portzero_tunnel_client::domain_router::DomainRouter;

use crate::auth::AuthConfig;
use crate::cloud::CloudConnector;
use crate::discovery;
use crate::docker_events::{self, ConflictRegistry};
use crate::net::overlay::{OverlayConfig, OverlayNetwork, ResolverCheck};
use crate::notify;
use crate::route_table::RouteTable;

/// Grace period before removing a route after its process exits.
/// Allows for quick process restarts without flapping.
const PROCESS_EXIT_GRACE_SECS: u64 = 5;

mod cloud_state;
mod config;
mod connection;
mod lifecycle;
mod overlay;
mod routes;

pub use cloud_state::{
    read_cloud_can_use_tunnels, read_cloud_can_use_tunnels_from_path, read_cloud_connected,
    read_cloud_error, read_cloud_message, read_cloud_message_from_path, read_cloud_plan,
    read_cloud_plan_from_path,
};
pub use config::DaemonConfig;
pub use lifecycle::{read_daemon_pid, remove_pid_file, stop_daemon};

use connection::{
    handle_cloud_auth_failure, initial_cloud_connect, maybe_connect_cloud, maybe_recheck_auth,
    maybe_refresh_token, register_persisted_routes, snapshot_cloud_state,
};
use lifecycle::{
    acquire_singleton_or_take_over, is_process_alive, spawn_shutdown_watchdog,
    wait_for_shutdown_signal, GRACEFUL_SHUTDOWN_TIMEOUT,
};
use overlay::{
    gather_and_publish_issues, notify_post_setup_regressions, refresh_overlay_services,
    run_dns_fast_overlay_refresh, write_overlay_state,
};
use routes::{log_changes, sync_cloud_routes, update_domain_router};

#[cfg(test)]
use std::path::PathBuf;

#[cfg(test)]
use crate::net::dns::DnsFirstHitPolicy;
#[cfg(test)]
use crate::net::stack::OverlayHttpsPolicy;
#[cfg(test)]
use crate::route_table::RouteChanges;

#[cfg(test)]
use cloud_state::write_cloud_state;
#[cfg(test)]
use config::FileConfig;
#[cfg(test)]
use overlay::build_overlay_table;

/// Tracks routes whose owning process has exited, giving them a grace period
/// before removal.
struct GracePeriodTracker {
    /// domain -> when the process was first noticed missing
    missing_since: HashMap<String, Instant>,
    grace_duration: Duration,
}

impl GracePeriodTracker {
    fn new() -> Self {
        Self {
            missing_since: HashMap::new(),
            grace_duration: Duration::from_secs(PROCESS_EXIT_GRACE_SECS),
        }
    }

    /// Record that a process for this domain is still missing.
    /// Returns true if the grace period has expired and the route should be removed.
    fn process_missing(&mut self, domain: &str) -> bool {
        let first_seen = self
            .missing_since
            .entry(domain.to_string())
            .or_insert_with(Instant::now);
        first_seen.elapsed() >= self.grace_duration
    }

    /// Record that a process for this domain is alive (clear grace period).
    fn process_alive(&mut self, domain: &str) {
        self.missing_since.remove(domain);
    }

    /// Clear all entries for domains that are no longer in the route table.
    fn prune(&mut self, active_domains: &[&String]) {
        self.missing_since
            .retain(|d, _| active_domains.contains(&d));
    }
}

/// Run the discovery loop until cancelled.
///
/// This is the main entry point for the daemon process. It:
/// 1. Writes a PID file
/// 2. Optionally connects to the cloud edge (if authenticated)
/// 3. Scans for services every `scan_interval_secs`
/// 4. Registers/unregisters routes with cloud as they change
/// 5. Checks for process exit with a grace period
pub async fn run_discovery_loop(config: &DaemonConfig) -> Result<()> {
    crate::install_default_crypto_provider();

    std::fs::create_dir_all(&config.state_dir).with_context(|| {
        format!(
            "Failed to create daemon state directory: {}\n\n\
             Check that you have write permissions to ~/.portzero/",
            config.state_dir.display()
        )
    })?;

    // Ensure we are the sole discovery daemon. If another live daemon is already
    // running, take over from it (SIGTERM, wait for it to release the TUN/DNS
    // resources, escalate to SIGKILL if it overstays) before claiming the PID
    // file. This runs BEFORE OverlayNetwork::start so the old daemon has released
    // the TUN device and DNS port by the time we create ours.
    acquire_singleton_or_take_over(config)?;

    let mut config = config.clone();

    let pid = std::process::id();
    tracing::info!("Discovery daemon started (PID {})", pid);
    tracing::info!(
        "Scanning every {}s, routes at {}",
        config.scan_interval_secs,
        config.routes_path().display()
    );

    // Set up domain router (shared with cloud connector)
    let domain_router = DomainRouter::new();

    // Load initial auth state and attempt cloud connection.
    let auth = AuthConfig::load();
    let mut current_token: Option<String> = auth.token.clone();
    let mut account_id: Option<String> = auth.account_id.clone();
    let mut username: Option<String> = auth.username.clone();
    // Blocks automatic reconnect attempts when the server has rejected our token.
    // Cleared when the token changes (i.e. the user re-authenticates).
    let mut auth_failed = false;

    // Records observed edges + exercised routes from forwarded cloud traffic
    // (task-63), persisted to observations.json for `portzero inspect` and the
    // MCP server. Shared with every cloud connector we create.
    let observations = Arc::new(crate::observations::ObservationStore::new(
        &config.state_dir,
    ));

    let mut cloud =
        initial_cloud_connect(&config, &current_token, &domain_router, &observations).await;

    let mut route_table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let mut grace_tracker = GracePeriodTracker::new();
    let mut reconnect_backoff = ReconnectBackoff::new();
    let mut next_auth_check = Instant::now() + Duration::from_secs(AUTH_CHECK_INTERVAL_SECS);
    let mut next_token_refresh_check =
        Instant::now() + Duration::from_secs(TOKEN_REFRESH_CHECK_INTERVAL_SECS);
    let mut next_config_check = Instant::now() + Duration::from_secs(CONFIG_RELOAD_INTERVAL_SECS);
    let mut last_config_mtime: Option<std::time::SystemTime> =
        std::fs::metadata(config.config_path())
            .ok()
            .and_then(|m| m.modified().ok());

    // Seed the domain router from any pre-existing routes
    for (domain, route) in &route_table.routes {
        domain_router.add_route(domain.clone(), route.port);
    }

    // The cloud connection is brand-new; register any routes that were
    // persisted from a previous daemon session so they are live immediately
    // without waiting for a scan cycle to detect a "change".
    register_persisted_routes(&cloud, &route_table).await;

    // Start the virtual overlay network (TUN + smoltcp stack + scoped DNS) for
    // services that use a full `*.portzero.local` name. This is independent of the
    // cloud tunnel: the overlay handles `.portzero.local`, cloud handles everything
    // else, so the two never conflict.
    //
    // Starting is best-effort: creating the TUN device requires root/CAP_NET_ADMIN
    // and will fail in unprivileged or CI environments. On failure we log a warning
    // and continue in cloud/local-only mode rather than aborting the daemon.
    // Tracks the last issue set we logged loudly / notified about, so repeated
    // scans of the same problem don't spam the log or the desktop. Seeded from
    // any state a previous daemon left behind so a restart doesn't re-notify for
    // an unchanged, still-present issue.
    let mut notified_issues = notify::read_issues(&config.issues_path());

    // Robust Docker event monitor (task-8): watches `docker events` in real time
    // for container start/die so we can (a) surface host-port-bind conflicts that
    // the periodic poll would miss (a failed start dies before the next scan) and
    // (b) rescan immediately instead of waiting the full poll interval. The
    // `ConflictRegistry` collects conflict issues the monitor finds so they can be
    // merged into the per-scan issue set below. `docker_rescan` lets the monitor
    // wake the loop early; `docker_shutdown` tears the monitor down cleanly.
    let conflicts = ConflictRegistry::new();
    let docker_rescan = Arc::new(Notify::new());
    let docker_shutdown = Arc::new(Notify::new());

    // Lets the embedded DNS server wake the discovery loop the instant it sees a
    // query for an unknown `*.portzero.local` name, so a just-started service is
    // discovered immediately instead of waiting for the next poll cycle.
    let dns_rescan = Arc::new(Notify::new());
    let docker_monitor = tokio::spawn(docker_events::run_event_monitor(
        conflicts.clone(),
        docker_rescan.clone(),
        docker_shutdown.clone(),
    ));

    let (mgmt_port, mgmt_store) =
        match crate::management::ManagementServer::bind(config.state_dir.clone()).await {
            Ok((mgmt_server, mgmt_listener)) => {
                let mgmt_port = mgmt_server.bound_port;
                let mgmt_store = mgmt_server.store.clone();
                // Fetch the getting-started examples in the background on startup so
                // the desktop app's first-run steps can offer a one-click example
                // run without the user ever thinking about downloading them.
                crate::management::handlers::spawn_auto_download(
                    mgmt_server.examples_download.clone(),
                );
                tokio::spawn(mgmt_server.serve(mgmt_listener));
                (mgmt_port, mgmt_store)
            }
            Err(e) => {
                tracing::warn!(
                    "Management API server could not bind; continuing without local API: {e:#}"
                );
                (0, Arc::new(tokio::sync::RwLock::new(HashMap::new())))
            }
        };

    // Overlay startup does privileged, platform-heavy work (trust-store install,
    // TUN/wintun device creation, scoped resolver setup) whose duration varies
    // widely — a first-run wintun driver install or slow storage can push it well
    // past 30s. Two hard requirements: it must NEVER block cloud tunnel discovery,
    // and a slow-but-SUCCESSFUL overlay must never be discarded. So we start it on
    // a blocking-pool thread and let the main loop ADOPT it whenever it finishes
    // (fast or slow) rather than deadlining it and tearing down a late success.
    // The per-phase progress log shows where a genuine wedge (if any) stops.
    let mut overlay_start_task: Option<tokio::task::JoinHandle<Result<OverlayNetwork>>> = {
        let overlay_config = OverlayConfig {
            https_policy: config.overlay_https,
            dns_first_hit_policy: config.dns_first_hit_policy,
            ..OverlayConfig::default()
        };
        let dns_rescan = dns_rescan.clone();
        let runtime = tokio::runtime::Handle::current();
        Some(tokio::task::spawn_blocking(move || {
            runtime.block_on(OverlayNetwork::start_with_progress(
                overlay_config,
                dns_rescan,
                |step| tracing::info!("overlay startup step: {step:?}"),
            ))
        }))
    };
    // `None` until the async startup task completes and the loop adopts it (see
    // the adoption block at the top of each iteration). The loop's normal
    // per-scan overlay refresh seeds services once adopted.
    let mut overlay: Option<Arc<OverlayNetwork>> = None;
    write_overlay_state(&config, &[], false);

    // Run an initial diagnostics scan so `status_json` has data from the moment
    // the daemon starts, without waiting for the first 30-second periodic check.
    {
        let diag = crate::diagnostics::run_diagnostics(&config.state_dir).await;
        crate::diagnostics::save_report(&diag, &config.state_dir);
        tracing::info!(
            "diagnostics: {} check(s) run, {} issue(s) found",
            diag.checks_run,
            diag.issues.len()
        );
    }

    // Long-lived shutdown future. We `select!` the ENTIRE loop body against this
    // every iteration so a SIGTERM/Ctrl-C is honoured immediately — even in the
    // middle of a (potentially slow, subprocess-heavy) scan — rather than only
    // in the gap between scans. `Box::pin` keeps the same future alive across
    // iterations so a signal that arrives mid-iteration is not lost when the
    // branch's local future is dropped.
    let mut shutdown = Box::pin(wait_for_shutdown_signal());

    let mut diag_scan_counter: u32 = 0;
    // Last legacy-listener sweep result, republished on the iterations between
    // the ~30s full-system sweeps (see gather_and_publish_issues).
    let mut legacy_issues_cache: Vec<notify::Issue> = Vec::new();
    // Whether we have already fired a desktop notification for the current
    // "resolver removed" episode. Reset once the resolver is healthy again so a
    // future removal notifies afresh (rather than once per 30s check).
    let mut resolver_repair_notified = false;
    // The set of post-setup regressions (sorted diagnostic IDs) we last notified
    // about, so a still-present, unchanged problem doesn't re-notify every 30s.
    // Reset to empty once everything setup-related is healthy again.
    let mut post_setup_notified: Vec<String> = Vec::new();
    // Started when (if) the overlay is adopted below, since the overlay is not
    // up yet at this point.
    let mut overlay_fast_refresh: Option<tokio::task::JoinHandle<()>> = None;
    // Tracks which HTTP/HTTPS local tunnels we've already popped in the browser
    // so a steady-state scan doesn't reopen them (see `auto_open`). Loaded from
    // disk so tunnels already running before this daemon (re)started are not
    // mistaken for freshly-appeared ones and reopened.
    let mut auto_open = crate::auto_open::AutoOpenTracker::load(&config.auto_open_path());

    loop {
        // The per-iteration work (scan + cloud sync + sleep) lives in this async
        // block so it can be raced against `shutdown` at the top level. If the
        // signal fires, this future is dropped at its next `.await` point and we
        // break out to teardown.
        let iteration = async {
            // Adopt the virtual overlay as soon as its (possibly slow) startup
            // finishes. This never blocks — we only `.await` a task already
            // reported finished. A slow-but-successful overlay is embraced here
            // rather than torn down; cloud tunnel discovery has been running
            // unblocked the entire time it was starting.
            adopt_overlay(
                &config,
                mgmt_port,
                &mgmt_store,
                &dns_rescan,
                &mut overlay,
                &mut overlay_start_task,
                &mut overlay_fast_refresh,
            )
            .await;

            // Re-run diagnostics every ~30s (every 15 × 2s scan cycles).
            diag_scan_counter = diag_scan_counter.wrapping_add(1);
            if diag_scan_counter.is_multiple_of(15) {
                run_periodic_diagnostics(
                    &config,
                    &overlay,
                    &mut resolver_repair_notified,
                    &mut post_setup_notified,
                )
                .await;
            }

            let (count, cloud_scope_issues) = reconcile_routes(
                &config,
                &mut route_table,
                &mut grace_tracker,
                account_id.as_deref(),
                username.as_deref(),
                &domain_router,
                &cloud,
            )
            .await;

            // Feed `.portzero.local` services into the virtual overlay. This is a
            // separate discovery pass from the cloud/route discovery above and does
            // not touch cloud tunnel registration. No-op when the overlay failed to
            // start (unprivileged environment).
            //
            // Also run the visibility checks (duplicate-name detection + legacy
            // listener monitoring) and publish the combined issue set once per scan.
            // Legacy monitoring runs regardless of whether the overlay started, so
            // when the overlay is unavailable we still scan with empty overlay data.
            let (overlay_issues, overlay_services) =
                refresh_overlay_and_scan(&config, &overlay, mgmt_port, &mgmt_store).await;
            // Pop a browser tab for any newly-appeared HTTP/HTTPS tunnel (e.g. a
            // just-started example) when the setting is on.
            auto_open.reconcile(config.auto_open_http_tunnels, &overlay_services);
            if let Err(err) = auto_open.save(&config.auto_open_path()) {
                tracing::debug!(%err, "failed to persist auto-open state");
            }
            let docker_conflicts = conflicts.snapshot().await;
            // Full-system legacy sweep only on the ~30s diagnostics cadence
            // (see gather_and_publish_issues); cached issues republish between
            // sweeps.
            let scan_legacy = diag_scan_counter.is_multiple_of(15);
            gather_and_publish_issues(
                &config,
                &route_table,
                overlay_issues,
                &overlay_services,
                docker_conflicts,
                cloud_scope_issues,
                &mut notified_issues,
                scan_legacy,
                &mut legacy_issues_cache,
            )
            .await;

            // Prune grace tracker
            let active_domains: Vec<&String> = route_table.routes.keys().collect();
            grace_tracker.prune(&active_domains);

            tracing::trace!(
                "Scan complete: {} services, {} routes",
                count,
                route_table.len()
            );

            // Periodic auth re-check: pick up login/logout without requiring a restart.
            if Instant::now() >= next_auth_check {
                next_auth_check = Instant::now() + Duration::from_secs(AUTH_CHECK_INTERVAL_SECS);
                maybe_recheck_auth(
                    &config,
                    &mut current_token,
                    &mut account_id,
                    &mut username,
                    &mut auth_failed,
                    &mut cloud,
                    &mut reconnect_backoff,
                );
            }

            // Periodic JWT token refresh: renew the token before it expires so
            // long-running daemons don't silently lose cloud connectivity.
            if Instant::now() >= next_token_refresh_check {
                next_token_refresh_check =
                    Instant::now() + Duration::from_secs(TOKEN_REFRESH_CHECK_INTERVAL_SECS);
                maybe_refresh_token(
                    &mut current_token,
                    &mut cloud,
                    &mut reconnect_backoff,
                    &mut auth_failed,
                )
                .await;
            }

            // Poll config file for changes (scan interval, overlay https policy,
            // dns first-hit policy). Changes are applied without restarting the
            // daemon and without dropping any active tunnel or overlay connections.
            if Instant::now() >= next_config_check {
                next_config_check =
                    Instant::now() + Duration::from_secs(CONFIG_RELOAD_INTERVAL_SECS);
                maybe_reload_config(&mut config, &overlay, &mut last_config_mtime).await;
            }

            snapshot_cloud_state(&config, &cloud);
            handle_cloud_auth_failure(&config, &mut cloud, &mut auth_failed);

            // Connect or reconnect whenever we have a token and are not blocked by an
            // auth failure.  This handles: initial connect retries, reconnects after
            // drops, and connecting after a fresh login while the daemon is running.
            maybe_connect_cloud(
                &config,
                &domain_router,
                &observations,
                &route_table,
                &current_token,
                auth_failed,
                &mut cloud,
                &mut reconnect_backoff,
            )
            .await;

            // Sleep until the next scan, waking early when the Docker event monitor
            // sees a relevant container start/die so a new (or vanished) container is
            // reflected promptly instead of waiting the full poll interval. The
            // shutdown signal is handled one level up (the top-level `select!`
            // below), so it can interrupt this sleep AND any of the scan work above.
            // Uses the (possibly reloaded) scan interval.
            let sleep_dur = Duration::from_secs(config.scan_interval_secs);
            tokio::select! {
                _ = tokio::time::sleep(sleep_dur) => {}
                _ = docker_rescan.notified() => {
                    tracing::trace!("Docker event triggered an immediate rescan");
                }
                _ = dns_rescan.notified() => {
                    tracing::trace!("DNS miss on an unknown subdomain triggered an immediate rescan");
                }
            }
        }; // end of `iteration` async block

        tokio::select! {
            _ = iteration => {}
            _ = &mut shutdown => {
                tracing::info!("Shutdown signal received, stopping discovery daemon");
                break;
            }
        }
    }

    // We have begun shutting down. From here on, a SECOND signal (or a stalled
    // graceful teardown) must force the process to exit so the operator never
    // has to `pkill`. Arm a watchdog that hard-exits on either condition.
    spawn_shutdown_watchdog();

    // Stop the Docker event monitor cleanly alongside the overlay teardown.
    docker_shutdown.notify_one();
    docker_monitor.abort();
    if let Some(task) = overlay_fast_refresh {
        task.abort();
    }
    // If the overlay was still starting up when we were asked to shut down,
    // abort the startup task so it can't create a TUN/resolver after teardown.
    if let Some(task) = overlay_start_task {
        task.abort();
    }

    // Graceful shutdown: tear down the overlay (removes the scoped resolver
    // config and the TUN device) before exiting so we don't leave the system's
    // DNS pointed at a dead server. Best-effort and time-bounded — if teardown
    // wedges (e.g. a blocking resolver uninstall), the watchdog above forces
    // exit, but we also cap the overlay teardown directly so the common case
    // returns promptly.
    if let Some(ov) = overlay {
        match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, ov.shutdown()).await {
            Ok(()) => tracing::info!("Virtual overlay network shut down"),
            Err(_) => tracing::warn!(
                "Overlay shutdown did not complete within {}s; continuing exit",
                GRACEFUL_SHUTDOWN_TIMEOUT.as_secs()
            ),
        }
    }

    // Persist any observed edges / exercised routes that were buffered since the
    // last throttled flush, so the final snapshot on disk is complete.
    observations.flush();

    remove_pid_file(&config);
    tracing::info!("Discovery daemon stopped");

    Ok(())
}

/// Adopt the virtual overlay as soon as its (possibly slow) startup finishes.
///
/// This never blocks — we only `.await` a task already reported finished. A
/// slow-but-successful overlay is embraced here rather than torn down; cloud
/// tunnel discovery has been running unblocked the entire time it was starting.
async fn adopt_overlay(
    config: &DaemonConfig,
    mgmt_port: u16,
    mgmt_store: &crate::management::RegistrationStore,
    dns_rescan: &Arc<Notify>,
    overlay: &mut Option<Arc<OverlayNetwork>>,
    overlay_start_task: &mut Option<tokio::task::JoinHandle<Result<OverlayNetwork>>>,
    overlay_fast_refresh: &mut Option<tokio::task::JoinHandle<()>>,
) {
    if overlay.is_some() || !overlay_start_task.as_ref().is_some_and(|t| t.is_finished()) {
        return;
    }
    match overlay_start_task
        .take()
        .expect("just checked is_some")
        .await
    {
        Ok(Ok(ov)) => {
            tracing::info!("Virtual overlay network started (.portzero.local)");
            let ov = Arc::new(ov);
            *overlay_fast_refresh = Some(tokio::spawn(run_dns_fast_overlay_refresh(
                config.clone(),
                ov.clone(),
                mgmt_port,
                mgmt_store.clone(),
                dns_rescan.clone(),
            )));
            *overlay = Some(ov);
        }
        Ok(Err(e)) => {
            tracing::warn!(
                "Virtual overlay network not started (continuing in \
                 cloud/local-only mode): {:#}. The overlay needs elevated \
                 privileges (root / CAP_NET_ADMIN) to create a TUN device.",
                e
            );
            write_overlay_state(config, &[], false);
        }
        Err(join_err) => tracing::warn!("overlay startup task panicked: {join_err}"),
    }
}

/// Re-run diagnostics and repair out-of-band regressions of setup steps.
///
/// Detects removal of the scoped `.portzero.local` resolver and recreates it,
/// and alarms (once) on any other post-setup step that has regressed. Both are
/// change-gated via `resolver_repair_notified` / `post_setup_notified` so an
/// unchanged problem doesn't re-notify every cycle.
async fn run_periodic_diagnostics(
    config: &DaemonConfig,
    overlay: &Option<Arc<OverlayNetwork>>,
    resolver_repair_notified: &mut bool,
    post_setup_notified: &mut Vec<String>,
) {
    let diag = crate::diagnostics::run_diagnostics(&config.state_dir).await;
    crate::diagnostics::save_report(&diag, &config.state_dir);

    if let Some(ov) = overlay {
        match ov.ensure_resolver_installed().await {
            ResolverCheck::Repaired => {
                if !*resolver_repair_notified {
                    notify::send_notification(
                        "portzero: DNS resolver restored",
                        "The .portzero.local resolver was removed and has \
                         been recreated automatically.",
                    );
                    *resolver_repair_notified = true;
                }
            }
            ResolverCheck::RepairFailed => {
                if !*resolver_repair_notified {
                    notify::send_notification(
                        "portzero: DNS resolver missing",
                        "The .portzero.local resolver was removed and could \
                         not be recreated. Run `sudo portzero setup` to fix it.",
                    );
                    *resolver_repair_notified = true;
                }
            }
            ResolverCheck::Ok => *resolver_repair_notified = false,
        }
    }

    // Gated on the overlay having started (a real privileged install) so a dev
    // running the daemon in the foreground is never nagged.
    if overlay.is_some() {
        notify_post_setup_regressions(&diag, post_setup_notified);
    }
}

/// Discover services, reconcile them against the route table (honouring the
/// process-exit grace period), and propagate any changes to disk, the domain
/// router, and the cloud connector.
///
/// Returns the discovered-service count and any cloud-scope issues found while
/// scanning, for the caller to fold into the per-scan issue set.
async fn reconcile_routes(
    config: &DaemonConfig,
    route_table: &mut RouteTable,
    grace_tracker: &mut GracePeriodTracker,
    account_id: Option<&str>,
    username: Option<&str>,
    domain_router: &DomainRouter,
    cloud: &Option<CloudConnector>,
) -> (usize, Vec<notify::Issue>) {
    let routes_snapshot: Vec<(String, u32)> = route_table
        .routes
        .iter()
        .map(|(d, r)| (d.clone(), r.pid))
        .collect();

    for (domain, route_pid) in &routes_snapshot {
        if is_process_alive(*route_pid) {
            grace_tracker.process_alive(domain);
        } else if grace_tracker.process_missing(domain) {
            tracing::info!(
                "Process {} for route {} exited (grace period expired), removing route",
                route_pid,
                domain
            );
        }
    }

    let (mut discovered, mut cloud_scope_issues) = discovery::scan_all(account_id, username).await;

    // Detect two or more contexts advertising the same cloud tunnel URL *before*
    // `RouteTable::update` collapses them to a single winning route (the losers
    // are silently dropped). Folded into the cloud-scope issue set so it flows
    // through the same publish/notify path as duplicate `.portzero.local` names.
    cloud_scope_issues.extend(notify::detect_duplicate_cloud_urls(&discovered));

    // Keep services whose processes have exited but whose grace period hasn't,
    // so a transient scan miss doesn't remove a still-live route.
    let grace_domains: Vec<String> = route_table
        .routes
        .iter()
        .filter(|(domain, route)| {
            if is_process_alive(route.pid) {
                // A live process cannot change its environment after exec, so a
                // scan that misses it (e.g. a `ps` failure under load) is
                // scanner noise, not a removed service. A fresh successful scan
                // of the same domain still wins over this re-injected copy in
                // RouteTable::update.
                matches!(
                    route.source,
                    crate::discovery::ServiceSource::Process { .. }
                )
            } else {
                !grace_tracker.process_missing(domain)
            }
        })
        .map(|(_, route)| route.domain.clone())
        .collect();

    for domain in &grace_domains {
        if let Some(route) = route_table.routes.get(domain) {
            discovered.push(crate::discovery::DiscoveredService {
                domain: route.domain.clone(),
                domain_template: route.domain_template.clone(),
                substitutions: route.substitutions.clone(),
                port: route.port,
                extra_ports: route.extra_ports.clone(),
                health_path: route.health_path.clone(),
                pid: route.pid,
                source: route.source.clone(),
            });
        }
    }

    let count = discovered.len();
    let changes = route_table.update(discovered);

    if changes.has_changes() {
        if let Err(e) = route_table.save(&config.routes_path()) {
            tracing::error!("Failed to save route table: {}", e);
        } else {
            log_changes(&changes);
        }
        update_domain_router(domain_router, &changes);
        if let Some(connector) = cloud {
            sync_cloud_routes(connector, &changes).await;
        }
    }

    (count, cloud_scope_issues)
}

/// Reload `config.toml` when it has changed on disk, applying scan-interval,
/// overlay HTTPS, and DNS first-hit policy updates live (no restart, no dropped
/// connections).
async fn maybe_reload_config(
    config: &mut DaemonConfig,
    overlay: &Option<Arc<OverlayNetwork>>,
    last_config_mtime: &mut Option<std::time::SystemTime>,
) {
    let current_mtime = std::fs::metadata(config.config_path())
        .ok()
        .and_then(|m| m.modified().ok());
    let changed = (current_mtime.is_some() && current_mtime != *last_config_mtime)
        || (last_config_mtime.is_some() && current_mtime.is_none());
    if !changed {
        if current_mtime.is_some() {
            *last_config_mtime = current_mtime;
        }
        return;
    }

    let reloaded = DaemonConfig::load();
    let changed_scan = reloaded.scan_interval_secs != config.scan_interval_secs;
    let changed_https = reloaded.overlay_https != config.overlay_https;
    let changed_dns = reloaded.dns_first_hit_policy != config.dns_first_hit_policy;

    if changed_scan || changed_https || changed_dns {
        tracing::info!("daemon config file changed; reloading");
    }
    if changed_scan {
        tracing::info!(
            "scan_interval_secs now {} (was {})",
            reloaded.scan_interval_secs,
            config.scan_interval_secs
        );
    }
    if changed_https {
        tracing::info!("overlay https policy updated");
        if let Some(ov) = overlay {
            if let Err(e) = ov.update_https_policy(reloaded.overlay_https).await {
                tracing::warn!(?e, "failed to push https policy to overlay stack");
            }
        }
    }
    if changed_dns {
        tracing::info!("dns_first_hit_policy updated");
        if let Some(ov) = overlay {
            ov.update_dns_first_hit_policy(reloaded.dns_first_hit_policy);
        }
    }
    *config = reloaded;
    *last_config_mtime = current_mtime;
}

/// Refresh the `.portzero.local` overlay services and return the freshly-scanned
/// duplicate-name issues and service list.
///
/// When the overlay is running, this also pushes the services into it; when it is
/// not (unprivileged environment), it still scans so `portzero status` can surface
/// discovered `.local` services. Both paths are time-bounded so a wedged scan
/// keeps the previous state rather than stalling the loop.
async fn refresh_overlay_and_scan(
    config: &DaemonConfig,
    overlay: &Option<Arc<OverlayNetwork>>,
    mgmt_port: u16,
    mgmt_store: &crate::management::RegistrationStore,
) -> (
    Vec<notify::Issue>,
    Vec<crate::discovery::DiscoveredNetworkService>,
) {
    if let Some(ov) = overlay {
        match tokio::time::timeout(
            OVERLAY_REFRESH_TIMEOUT,
            refresh_overlay_services(ov, mgmt_port, mgmt_store),
        )
        .await
        {
            Ok(result) => {
                write_overlay_state(config, &result.1, true);
                result
            }
            Err(_) => {
                tracing::warn!("Overlay service refresh timed out; keeping previous state");
                (Vec::new(), Vec::new())
            }
        }
    } else {
        let services = match tokio::time::timeout(
            OVERLAY_REFRESH_TIMEOUT,
            discovery::scan_network_services(mgmt_store),
        )
        .await
        {
            Ok(services) => services,
            Err(_) => {
                tracing::warn!("Overlay service scan timed out; keeping previous state");
                Vec::new()
            }
        };
        let issues = notify::detect_duplicate_names(&services);
        write_overlay_state(config, &services, false);
        (issues, services)
    }
}

#[cfg(test)]
mod tests;
