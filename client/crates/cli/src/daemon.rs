//! Daemon control commands: start, stop, status, restart.

use std::time::Duration;

use anyhow::{Context, Result};

use portzero_daemon::discovery_loop::{
    read_cloud_can_use_tunnels, read_cloud_connected, read_cloud_error, read_cloud_message,
    read_cloud_plan, read_daemon_pid, DaemonConfig,
};
use portzero_daemon::notify::read_issues;
use portzero_daemon::route_table::{OverlayState, RouteTable};

use crate::auth::AuthConfig;

/// Start the discovery daemon in the background.
///
/// If the daemon is already running, prints its PID and exits. Otherwise
/// spawns a new background process and then self-verifies the two conditions
/// behind the #1 first-run failure (overlay active, `.portzero.local` DNS
/// resolution) so an unprivileged start warns immediately instead of
/// reporting a false success.
pub async fn start(open_browser: bool) -> Result<()> {
    let config = DaemonConfig::load();

    // Check if already running.
    if let Some(pid) = read_daemon_pid(&config) {
        println!("Daemon is already running (PID {pid}).");
        open_desktop_app(open_browser);
        return Ok(());
    }

    let spawned = spawn_daemon(&config)?;

    if spawned.confirmed {
        let auth_status = if AuthConfig::load().is_ok() {
            "authenticated (tunnel will connect to cloud)"
        } else {
            "not authenticated (local-only mode, run `portzero login` for cloud tunnels)"
        };

        println!("Daemon started (PID {}).", spawned.pid);
        println!("Auth: {auth_status}");
        println!("Log:  {}", spawned.log_path.display());
        open_desktop_app(open_browser);
        verify_first_run(&config).await;
    } else {
        println!(
            "Daemon process spawned (PID {}) but did not confirm startup.\n\
             Check the log for errors: {}",
            spawned.pid,
            spawned.log_path.display()
        );
    }

    Ok(())
}

/// Outcome of spawning the daemon background process.
pub(crate) struct SpawnedDaemon {
    /// PID of the spawned process.
    pub(crate) pid: u32,
    /// Where the daemon's stdout/stderr are appended.
    pub(crate) log_path: std::path::PathBuf,
    /// Whether the daemon wrote its PID file within the startup window.
    pub(crate) confirmed: bool,
}

/// Spawn the daemon as a background process (re-invoking this binary with the
/// hidden `start --foreground`) and wait briefly for it to confirm startup by
/// writing its PID file. Shared by `portzero start` and `portzero demo`.
pub(crate) fn spawn_daemon(config: &DaemonConfig) -> Result<SpawnedDaemon> {
    // Ensure state directory exists.
    std::fs::create_dir_all(&config.state_dir).with_context(|| {
        format!(
            "Failed to create daemon state directory: {}\n\n\
             Check that you have write permissions to ~/.portzero/",
            config.state_dir.display()
        )
    })?;

    // Find our own executable so we can re-invoke with `start --foreground` style,
    // but since the skeleton uses `run_discovery_loop` directly, we spawn the
    // current binary with an internal flag.
    let exe = std::env::current_exe().with_context(|| {
        "Could not determine the path to the portzero binary.\n\n\
         Try running with an absolute path, e.g. /usr/local/bin/portzero start"
    })?;

    let log_path = config.log_path();

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| {
            format!(
                "Failed to open daemon log file: {}\n\n\
                 Check permissions on ~/.portzero/daemon/",
                log_path.display()
            )
        })?;

    let stderr_file = log_file
        .try_clone()
        .context("Failed to clone log file handle")?;

    let child = std::process::Command::new(&exe)
        .arg("start")
        .arg("--foreground")
        .stdout(log_file)
        .stderr(stderr_file)
        .stdin(std::process::Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "Failed to spawn daemon process from: {}\n\n\
                 Is the portzero binary executable?",
                exe.display()
            )
        })?;

    let child_pid = child.id();

    // Wait briefly for the daemon to write its PID file, confirming startup.
    let mut started = false;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        if read_daemon_pid(config).is_some() {
            started = true;
            break;
        }
    }

    Ok(SpawnedDaemon {
        pid: child_pid,
        log_path,
        confirmed: started,
    })
}

/// Post-start self-verification: catch the #1 first-run failure (daemon
/// reports "running" while the overlay never came up because it started
/// unprivileged) right at `portzero start` time, instead of letting the user
/// walk away believing everything works. Reuses the doctor's checks; on the
/// healthy path this adds well under a second.
async fn verify_first_run(config: &DaemonConfig) {
    let overlay = wait_for_overlay(config, Duration::from_secs(2)).await;
    let overlay_check = crate::doctor::check_overlay_active(read_daemon_pid(config), &overlay);

    if overlay_check.is_pass() {
        // Only probe DNS when the overlay is up: with the overlay down the
        // probe cannot pass and would only add its timeout to `start`.
        let dns_check = crate::doctor::check_os_resolution().await;
        if !dns_check.is_pass() {
            println!();
            crate::doctor::print_alert(&dns_check);
            println!("       Run `portzero doctor` for a full diagnosis.");
        }
    } else {
        println!();
        crate::doctor::print_alert(&overlay_check);
        println!("       Run `portzero doctor` for a full diagnosis.");
    }

    if crate::export::discovered_tunnels(config).is_empty() {
        println!();
        println!(
            "No tunnels yet — run `portzero demo` to see a working Local tunnel in one command."
        );
    }
}

/// Poll `overlay.json` briefly, waiting for the freshly started daemon to
/// bring the overlay up. The TUN device comes up moments after the PID file
/// is written; give it up to `max_wait` so a healthy start doesn't print a
/// spurious warning, without noticeably slowing `portzero start` down.
async fn wait_for_overlay(config: &DaemonConfig, max_wait: Duration) -> OverlayState {
    let deadline = std::time::Instant::now() + max_wait;
    loop {
        let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();
        if overlay.overlay_active || std::time::Instant::now() >= deadline {
            return overlay;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Open the PortZero desktop app — the primary local GUI, which replaces the
/// old browser dashboard at `http://portzero.local`. `--no-browser` (i.e.
/// `open_browser == false`) skips launching it, matching the previous behaviour
/// for the browser.
fn open_desktop_app(open_browser: bool) {
    if !open_browser {
        println!("PortZero app launch skipped (--no-browser).");
        println!("Open it any time from your applications menu, or run `portzero-app`.");
        return;
    }
    match portzero_domain::app::launch() {
        Ok(()) => println!("Opening the PortZero app..."),
        Err(e) => {
            println!("Could not open the PortZero app automatically: {e}");
            println!(
                "Launch it from your applications menu, or run `portzero-app`. \
                 If it isn't installed, reinstalling PortZero adds it (or set \
                 PORTZERO_APP_BIN to its full path)."
            );
        }
    }
}

/// Run the daemon in the foreground (called internally by `start --foreground`).
pub async fn start_foreground() -> Result<()> {
    let config = DaemonConfig::load();
    portzero_daemon::discovery_loop::run_discovery_loop(&config).await
}

/// Stop the daemon.
pub fn stop() -> Result<()> {
    let config = DaemonConfig::load();
    portzero_daemon::discovery_loop::stop_daemon(&config)?;
    println!("Daemon stopped.");
    Ok(())
}

/// Show daemon and route status.
pub async fn status() -> Result<()> {
    let config = DaemonConfig::load();

    match read_daemon_pid(&config) {
        Some(pid) => {
            println!("Daemon: running (PID {pid})");
        }
        None => {
            println!("Daemon: stopped");
            println!("\nRun `portzero start` to begin discovering local tunnels.");
            return Ok(());
        }
    }

    print_issues(&config);

    // Auth / tunnel status — verify the token against the API so we catch
    // expired sessions instead of showing a stale "logged in" from the local file.
    match AuthConfig::load() {
        Ok(auth) => {
            let token_ok = verify_token().await;
            match token_ok {
                TokenStatus::Valid => {
                    println!("Auth:   logged in as {}", auth.email);
                }
                TokenStatus::Expired => {
                    println!("Auth:   session expired — run `portzero login`");
                    println!("Tunnel: routes will return 502 until you re-authenticate");
                    print_routes(&config);
                    return Ok(());
                }
                TokenStatus::Unreachable => {
                    println!(
                        "Auth:   logged in as {} (could not verify — offline?)",
                        auth.email
                    );
                }
            }
            let edge_display = portzero_domain::endpoints::edge_url();
            let cloud_err = read_cloud_error(&config);
            let is_auth_err = cloud_err
                .as_deref()
                .map(|e| e.contains("Authentication failed"))
                .unwrap_or(false);
            match read_cloud_connected(&config) {
                Some(true) => println!("Tunnel: connected to {edge_display}"),
                Some(false) if is_auth_err => {
                    println!("Tunnel: disconnected (auth token rejected — run `portzero login`)");
                }
                Some(false) => {
                    if let Some(err) = cloud_err {
                        println!("Tunnel: disconnected — {}", err);
                    } else {
                        println!("Tunnel: disconnected — cannot reach {edge_display}");
                    }
                }
                None => println!("Tunnel: connecting to {edge_display}"),
            }

            if let Some(p) = read_cloud_plan(&config) {
                println!("Plan:   {}", p);
            }
            if let Some(msg) = read_cloud_message(&config) {
                println!();
                println!("  {}", msg);
                println!("  Upgrade: https://app.portzero.cloud");
                println!();
            } else if read_cloud_can_use_tunnels(&config) == Some(false) {
                // Gentle upsell if we know the account can't use cloud tunnels (own plan
                // and any team it belongs to are both free) and the edge hasn't sent an
                // explicit message yet.
                println!("        (Cloud tunnels require a paid plan or a paid team. Run `portzero whoami` or visit https://portzero.net/#pricing)");
            }
        }
        Err(_) => {
            println!("Auth:   not logged in");
            println!("Tunnel: disabled (run `portzero login` for cloud tunnels)");
        }
    }

    print_routes(&config);

    Ok(())
}

/// Surface any current visibility issues (e.g. duplicate `.portzero.local` names
/// claimed by multiple worktrees) recorded by the running daemon.
///
/// `LegacyListener` issues are excluded — they are log-only (see
/// `discovery_loop::overlay::publish_issues` and `notify::collect_problems`),
/// not user-facing problems, so `portzero status` must not print them either.
fn print_issues(config: &DaemonConfig) {
    let state = read_issues(&config.issues_path());
    let issues: Vec<_> = state
        .issues
        .iter()
        .filter(|issue| !matches!(issue, portzero_daemon::notify::Issue::LegacyListener { .. }))
        .collect();
    if issues.is_empty() {
        return;
    }

    println!();
    println!(
        "Issues: {} problem(s) detected — see fixes below:",
        issues.len()
    );
    for issue in &issues {
        println!("  ! {}", issue.summary());
        println!("    fix: {}", issue.fix_hint());
    }
}

fn print_routes(config: &DaemonConfig) {
    let table = RouteTable::load(&config.routes_path()).unwrap_or_default();
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();

    if table.is_empty() && overlay.is_empty() {
        println!("\nNo routes discovered yet.");
        println!("{}", crate::pz_tunnel_contract());
        return;
    }

    // Group overlay routes by domain so a contested name shows the backend that
    // actually serves it — not an opaque `[CONFLICT]` row, and not one arbitrary
    // claimant with the rest silently dropped.
    let claims = portzero_daemon::claims::group_by_claim(overlay.routes.iter());

    // Columns: (domain, port, health-path, source).
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    let mut contested: Vec<&String> = Vec::new();

    for route in table.routes.values() {
        rows.push((
            route.domain.clone(),
            route.port.to_string(),
            health_cell(route.health_path.as_deref()),
            format_source(&route.source, route.pid),
        ));
    }

    for (domain, claimants) in &claims {
        let serving = claimants[0];
        let source = if claimants.len() > 1 {
            contested.push(domain);
            format!(
                "{} [serving; {} shadowed]",
                format_source(&serving.source, serving.pid),
                claimants.len() - 1
            )
        } else {
            format_source(&serving.source, serving.pid)
        };
        rows.push((
            domain.clone(),
            serving.service_port.to_string(),
            health_cell(serving.health_path.as_deref()),
            source,
        ));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));

    let max_domain = rows
        .iter()
        .map(|(d, _, _, _)| d.len())
        .max()
        .unwrap_or(0)
        .max("DOMAIN".len());
    let max_port = rows
        .iter()
        .map(|(_, p, _, _)| p.len())
        .max()
        .unwrap_or(0)
        .max("PORT".len());
    // Only show a HEALTH column when at least one route declares a health path,
    // so the common (no PZ_HEALTH_PATH) case stays uncluttered.
    let any_health = rows.iter().any(|(_, _, h, _)| h != "-");
    let max_health = rows
        .iter()
        .map(|(_, _, h, _)| h.len())
        .max()
        .unwrap_or(0)
        .max("HEALTH".len());

    println!();
    if any_health {
        println!(
            "{:<domain_w$}  {:<port_w$}  {:<health_w$}  SOURCE",
            "DOMAIN",
            "PORT",
            "HEALTH",
            domain_w = max_domain,
            port_w = max_port,
            health_w = max_health,
        );
        for (domain, port, health, source) in &rows {
            println!(
                "{:<domain_w$}  {:<port_w$}  {:<health_w$}  {}",
                domain,
                port,
                health,
                source,
                domain_w = max_domain,
                port_w = max_port,
                health_w = max_health,
            );
        }
    } else {
        println!(
            "{:<domain_w$}  {:<port_w$}  SOURCE",
            "DOMAIN",
            "PORT",
            domain_w = max_domain,
            port_w = max_port,
        );
        for (domain, port, _health, source) in &rows {
            println!(
                "{:<domain_w$}  {:<port_w$}  {}",
                domain,
                port,
                source,
                domain_w = max_domain,
                port_w = max_port,
            );
        }
    }

    // Print the detail block for every contested name after the table. Naming
    // the serving backend is the point: without it, a stale leftover can answer
    // requests and the resulting errors look like application bugs.
    if !contested.is_empty() {
        println!();
        println!("Ambiguous names — more than one live backend claims these tunnels:");
        for domain in &contested {
            let claimants = &claims[*domain];
            println!();
            println!("  ! {domain} claimed by {} backends:", claimants.len());
            for (index, claimant) in claimants.iter().enumerate() {
                println!(
                    "      - {} on {} [{}]",
                    format_source(&claimant.source, claimant.pid),
                    claimant.real_addr,
                    portzero_daemon::claims::standing(index),
                );
            }
        }
        println!();
        println!(
            "  Only the [serving] backend receives traffic. If the others are leftovers\n  \
             from an earlier run, stop them by pid. If they are meant to coexist, give each\n  \
             a unique PZ_TUNNEL value, e.g. PZ_TUNNEL=web-{{branch}}.portzero.local"
        );
    }

    if !overlay.is_empty() && !overlay.overlay_active {
        println!();
        println!("{}", overlay_inactive_hint());
    }
}

fn overlay_inactive_hint() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Note: .portzero.local tunnels are not reachable — the overlay network requires\n\
         Administrator privileges and wintun.dll next to portzero.exe or in PATH."
    }
    #[cfg(target_os = "linux")]
    {
        "Note: .portzero.local tunnels are not reachable — the overlay network requires\n\
         CAP_NET_ADMIN and CAP_NET_BIND_SERVICE. Grant both capabilities:\n\
         \n  sudo setcap 'cap_net_admin,cap_net_bind_service+eip' $(which portzero)"
    }
    #[cfg(target_os = "macos")]
    {
        "Note: .portzero.local tunnels are not reachable — the overlay network requires\n\
         root privileges or the networking Network Extension entitlement."
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        "Note: .portzero.local tunnels are not reachable — the overlay network requires\n\
         platform-specific privileges to create a TUN device and configure DNS."
    }
}

/// Render the HEALTH column cell for a route: its health path, or `-` when the
/// endpoint declared no `PZ_HEALTH_PATH`.
fn health_cell(health_path: Option<&str>) -> String {
    health_path.unwrap_or("-").to_string()
}

fn format_source(source: &portzero_daemon::discovery::ServiceSource, pid: u32) -> String {
    match source {
        portzero_daemon::discovery::ServiceSource::Process { .. } => {
            format!("PID {pid}")
        }
        portzero_daemon::discovery::ServiceSource::Container { id, .. } => {
            format!("container {}", &id[..id.len().min(12)])
        }
    }
}

enum TokenStatus {
    Valid,
    Expired,
    Unreachable,
}

async fn verify_token() -> TokenStatus {
    use crate::api_client::ApiClient;
    use std::time::Duration;

    let client = ApiClient::new();
    let result = tokio::time::timeout(Duration::from_secs(5), client.get("/auth/me")).await;

    match result {
        Ok(Ok(resp)) => {
            if resp.status().as_u16() == 401 {
                TokenStatus::Expired
            } else if resp.status().is_success() {
                TokenStatus::Valid
            } else {
                TokenStatus::Unreachable
            }
        }
        Ok(Err(_)) | Err(_) => TokenStatus::Unreachable,
    }
}

/// Restart the daemon (stop + start).
pub async fn restart() -> Result<()> {
    let config = DaemonConfig::load();

    // Stop if running, ignore error if not.
    if read_daemon_pid(&config).is_some() {
        portzero_daemon::discovery_loop::stop_daemon(&config).ok();
        // Brief pause to let the process exit.
        std::thread::sleep(Duration::from_millis(500));
    }

    start(true).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use portzero_daemon::discovery::ServiceSource;

    #[test]
    fn health_cell_shows_dash_when_no_health_path() {
        assert_eq!(health_cell(None), "-");
    }

    #[test]
    fn health_cell_shows_declared_path() {
        assert_eq!(health_cell(Some("/healthz")), "/healthz");
    }

    #[test]
    fn format_source_process_shows_pid() {
        let source = ServiceSource::Process { cwd: None };
        assert_eq!(format_source(&source, 4242), "PID 4242");
    }

    #[test]
    fn format_source_process_ignores_cwd() {
        let source = ServiceSource::Process {
            cwd: Some(std::path::PathBuf::from("/home/user/app")),
        };
        assert_eq!(format_source(&source, 1), "PID 1");
    }

    #[test]
    fn format_source_container_truncates_id_to_12_chars() {
        let source = ServiceSource::Container {
            id: "abcdef0123456789fulllength".to_string(),
            name: "web".to_string(),
        };
        assert_eq!(format_source(&source, 0), "container abcdef012345");
    }

    #[test]
    fn format_source_container_short_id_is_not_padded() {
        let source = ServiceSource::Container {
            id: "ab12".to_string(),
            name: "web".to_string(),
        };
        assert_eq!(format_source(&source, 0), "container ab12");
    }

    // `overlay_inactive_hint` is `#[cfg(target_os = ...)]`-gated internally; only
    // the branch for the OS we're compiling/testing on is present, so we can only
    // assert the shape of whatever variant is active here.
    #[test]
    fn overlay_inactive_hint_is_nonempty_and_mentions_overlay() {
        let hint = overlay_inactive_hint();
        assert!(!hint.is_empty());
        assert!(hint.contains("overlay network"));
    }
}
