//! Side-effecting actions the tray can take: control the daemon, open URLs, and
//! flip the HTTPS policy.
//!
//! Daemon lifecycle is driven by shelling out to the `portzero` CLI (`portzero
//! start` / `stop` / `restart`) rather than re-implementing it, so the tray and
//! the CLI always agree on how the daemon is spawned, and the tray never needs
//! elevated privileges of its own. The HTTPS toggle is written straight to
//! `config.toml`; a running daemon polls that file and hot-applies the change
//! within a couple of seconds, and a stopped daemon picks it up on next start.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use portzero_daemon::discovery_loop::DaemonConfig;
use portzero_daemon::net::stack::OverlayHttpsPolicy;

/// Locate the `portzero` CLI binary.
///
/// Prefers `PORTZERO_BIN`, then a sibling of this tray binary (the layout every
/// installer produces), and finally bare `portzero` on `PATH`.
fn portzero_bin() -> PathBuf {
    if let Some(explicit) = std::env::var_os("PORTZERO_BIN") {
        return PathBuf::from(explicit);
    }
    #[cfg(windows)]
    const EXE: &str = "portzero.exe";
    #[cfg(not(windows))]
    const EXE: &str = "portzero";

    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            let sibling = dir.join(EXE);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("portzero")
}

/// Run `portzero <sub>` detached, returning once the CLI has been spawned. The
/// CLI itself daemonizes, so we do not wait for it to exit.
fn run_portzero(sub: &str) -> Result<()> {
    let bin = portzero_bin();
    Command::new(&bin)
        .arg(sub)
        // Never pop a browser when the tray drives the daemon.
        .args(if sub == "start" || sub == "restart" {
            vec!["--no-browser"]
        } else {
            vec![]
        })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to launch `{} {sub}`", bin.display()))?;
    Ok(())
}

/// Start the daemon (`portzero start`).
pub fn start_daemon() -> Result<()> {
    run_portzero("start")
}

/// Stop the daemon (`portzero stop`).
pub fn stop_daemon() -> Result<()> {
    run_portzero("stop")
}

/// Restart the daemon (`portzero restart`).
pub fn restart_daemon() -> Result<()> {
    run_portzero("restart")
}

/// Launch the PortZero desktop app (best-effort, non-blocking).
///
/// This replaces the old "open the browser at `http://portzero.local`" action:
/// the app is now the primary local GUI. Locating and spawning it goes through
/// the shared launcher in `portzero_domain::app`, so the tray and the CLI open
/// the exact same binary the same way. Thanks to the app's single-instance
/// guard, launching while it is already open just focuses its window.
pub fn open_app() -> Result<()> {
    portzero_domain::app::launch().context(
        "failed to launch the PortZero app. Make sure it is installed \
         (reinstalling PortZero usually fixes this), or set PORTZERO_APP_BIN to \
         its full path.",
    )
}

/// Open a URL in the user's default browser (best-effort, non-blocking).
pub fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let (program, args): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let (program, args): (&str, Vec<&str>) = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, args): (&str, Vec<&str>) = ("xdg-open", vec![url]);

    Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to open {url}"))?;
    Ok(())
}

/// Persist a new HTTPS policy to `config.toml`. A running daemon hot-applies it
/// within a few seconds; a stopped daemon applies it on next start.
pub fn set_https_enabled(config: &DaemonConfig, enabled: bool) -> Result<()> {
    let mut policy: OverlayHttpsPolicy = DaemonConfig::load().overlay_https;
    policy.enable_for_port_80 = enabled;
    config
        .write_https_policy(policy)
        .context("failed to write HTTPS policy to config.toml")
}
