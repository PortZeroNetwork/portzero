//! Daemon process lifecycle: the PID file, singleton takeover from a prior
//! daemon, termination-signal handling, and the shutdown watchdog.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::config::DaemonConfig;

/// Hard upper bound on how long the graceful teardown may take before we force
/// the process to exit. Keeps Ctrl-C / SIGTERM responsive even if a teardown
/// step wedges.
pub(super) const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Arm a safety net so shutdown can never wedge the process.
///
/// A hard deadline (`GRACEFUL_SHUTDOWN_TIMEOUT`) forces `process::exit(0)` even
/// if tokio's blocking-thread pool is still draining (e.g. an in-flight
/// spawn_blocking scan). Uses a plain OS thread so it survives the tokio runtime
/// being torn down when the `main()` future returns.
pub(super) fn spawn_shutdown_watchdog() {
    std::thread::spawn(|| {
        std::thread::sleep(GRACEFUL_SHUTDOWN_TIMEOUT);
        tracing::warn!(
            "Graceful shutdown exceeded {}s, forcing exit",
            GRACEFUL_SHUTDOWN_TIMEOUT.as_secs()
        );
        std::process::exit(0);
    });
}

/// Wait for a termination signal (SIGTERM or Ctrl-C) so the loop can shut down
/// gracefully and tear down the overlay/TUN. Resolves when either fires.
pub(super) async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to install SIGTERM handler: {}", e);
                // Fall back to ctrl_c only.
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Check if a process is still alive by PID.
pub(super) fn is_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    use sysinfo::{Pid, System};
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    sys.process(Pid::from_u32(pid)).is_some()
}

/// Send a stop signal to `pid`. With `force`, escalates to SIGKILL (`taskkill /F`
/// on Windows); otherwise a graceful SIGTERM (Unix). Best-effort: only a failure
/// to launch the kill command surfaces as an error.
fn signal_pid(pid: u32, force: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::process::Command;
        let sig = if force { "-KILL" } else { "-TERM" };
        Command::new("kill")
            .args([sig, &pid.to_string()])
            .status()
            .with_context(|| format!("Failed to signal daemon process {pid}"))?;
    }
    #[cfg(windows)]
    {
        use std::process::Command;
        // Windows has no graceful console signal we can reliably deliver to a
        // detached process, so we terminate it directly in both cases.
        let _ = force;
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .status()
            .with_context(|| format!("Failed to terminate daemon process {pid}"))?;
    }
    Ok(())
}

/// How long to wait for an existing daemon to exit after we ask it to stop,
/// before escalating to SIGKILL. Slightly longer than the old daemon's own
/// graceful-teardown budget ([`GRACEFUL_SHUTDOWN_TIMEOUT`] = 5s) plus its
/// shutdown watchdog, so a cleanly-exiting daemon is never force-killed.
const TAKEOVER_TIMEOUT: Duration = Duration::from_secs(8);

/// Ensure this process is the sole discovery daemon, taking over from any
/// existing one, then claim the PID file.
///
/// If a live daemon is recorded in the PID file (and it isn't us), we ask it to
/// stop, wait for it to exit and release the TUN/DNS resources, and escalate to
/// SIGKILL if it overstays [`TAKEOVER_TIMEOUT`]. This makes `start --foreground`
/// — and therefore systemd restarts, `just install`, and manual launches —
/// self-correcting: the newest daemon always wins, instead of silently running
/// alongside an orphaned older one.
pub(super) fn acquire_singleton_or_take_over(config: &DaemonConfig) -> Result<()> {
    let me = std::process::id();

    if let Some(other) = read_daemon_pid(config) {
        if other != me {
            tracing::warn!(
                "Another discovery daemon (PID {other}) is already running; taking over"
            );
            if let Err(e) = signal_pid(other, false) {
                tracing::warn!("Failed to signal existing daemon {other}: {e:#}");
            }

            let deadline = Instant::now() + TAKEOVER_TIMEOUT;
            while crate::management::pid_lookup::pid_is_alive(other) {
                if Instant::now() >= deadline {
                    tracing::warn!(
                        "Existing daemon {other} did not exit within {}s; sending SIGKILL",
                        TAKEOVER_TIMEOUT.as_secs()
                    );
                    let _ = signal_pid(other, true);
                    // Give the kernel a moment to reap it and release the TUN/port.
                    std::thread::sleep(Duration::from_millis(500));
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            tracing::info!("Previous daemon {other} has exited; claiming ownership");
        }
    }

    std::fs::write(config.pid_path(), me.to_string()).with_context(|| {
        format!(
            "Failed to write PID file: {}\n\n\
             Check write permissions on the daemon state directory.",
            config.pid_path().display()
        )
    })?;
    Ok(())
}

/// Read the daemon PID from the PID file. Returns None if not found or stale.
pub fn read_daemon_pid(config: &DaemonConfig) -> Option<u32> {
    let pid_path = config.pid_path();
    let content = std::fs::read_to_string(&pid_path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;

    if is_process_alive(pid) {
        Some(pid)
    } else {
        // Stale PID file, clean it up
        let _ = std::fs::remove_file(&pid_path);
        None
    }
}

/// Remove the PID file (on clean shutdown).
pub fn remove_pid_file(config: &DaemonConfig) {
    let _ = std::fs::remove_file(config.pid_path());
}

/// Stop the discovery daemon by sending SIGTERM (Unix) or terminating (Windows).
pub fn stop_daemon(config: &DaemonConfig) -> Result<()> {
    let pid = read_daemon_pid(config);

    if let Some(pid) = pid {
        signal_pid(pid, false)?;
        remove_pid_file(config);
    }

    // Windows only gets `taskkill /F` (see signal_pid), so the daemon's
    // graceful overlay teardown — which removes the .portzero.local NRPT rule —
    // never runs. Remove the rule here, best-effort, AFTER the daemon is gone
    // (its resolver-repair loop would otherwise recreate it). Run it even when
    // no daemon was found so `stop` (and the MSI uninstall custom action that
    // invokes it) always clears leftover DNS residue.
    #[cfg(windows)]
    if let Err(e) = crate::net::resolver_config::remove_nrpt_rule_sync() {
        tracing::warn!(
            "could not remove the .portzero.local NRPT rule (may need administrator): {e:#}"
        );
    }

    if pid.is_none() {
        anyhow::bail!(
            "Discovery daemon is not running.\n\n\
             Start it with: portzero start"
        );
    }

    Ok(())
}
