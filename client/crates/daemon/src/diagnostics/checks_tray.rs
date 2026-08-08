//! Tray-presence diagnostics.
//!
//! Split out of `checks.rs` to keep both files inside the 1000-line budget,
//! alongside `checks_tls_trust.rs`. One question: is the system-tray companion
//! actually there when the user asked for it to be?

// Only the Linux and macOS autostart probes look at paths; Windows infers
// autostart from the installed binary instead (see `tray_autostart_installed`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;

use super::{Diagnostic, Fix, FixKind, Severity};

/// Whether the tray is there, from the user's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayPresence {
    /// A `portzero-tray` process is running, so there should be an icon.
    Running(u32),
    /// The tray is registered to start at login but nothing is running.
    Missing,
    /// The tray was never set up to start at login, so its absence is normal.
    NotConfigured,
}

/// Establish whether the tray is actually running, and whether it was meant to.
///
/// Shared by the daemon's diagnostics report and `portzero doctor` so the two
/// cannot disagree about something this visible.
pub fn tray_presence() -> TrayPresence {
    match tray_process_pid() {
        Some(pid) => TrayPresence::Running(pid),
        None if tray_autostart_installed() => TrayPresence::Missing,
        None => TrayPresence::NotConfigured,
    }
}

/// PID of a running `portzero-tray`, if there is one.
///
/// Deliberately a process scan rather than a read of `~/.portzero/tray.pid` or
/// the component record: those files are what go wrong when the tray is missing,
/// so a check that trusts them cannot detect the failure it exists to catch.
fn tray_process_pid() -> Option<u32> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    sys.processes()
        .iter()
        // `thread_kind()` is `Some` for a thread and `None` for a process. On
        // Linux sysinfo enumerates threads alongside processes, and a thread
        // carries its parent's name — so without this the tray's own worker
        // thread can win the search and we report its thread id as the tray's
        // PID.
        .find(|(_, p)| {
            p.thread_kind().is_none() && {
                let name = p.name().to_string_lossy();
                name.strip_suffix(".exe").unwrap_or(&name) == "portzero-tray"
            }
        })
        .map(|(pid, _)| pid.as_u32())
}

/// Is the tray registered to start at login?
///
/// Only worth reporting a missing tray to someone who asked for one — the tray
/// is an opt-in companion, and a user who never installed it (or deliberately
/// quit it for this session) should not be nagged.
fn tray_autostart_installed() -> bool {
    #[cfg(target_os = "linux")]
    {
        // Package installs land the XDG entry system-wide; the shell installer
        // and `just` write a per-user copy.
        let system = Path::new("/etc/xdg/autostart/portzero-tray.desktop").exists();
        let user = std::env::var_os("HOME").is_some_and(|home| {
            Path::new(&home)
                .join(".config/autostart/portzero-tray.desktop")
                .exists()
        });
        system || user
    }
    #[cfg(target_os = "macos")]
    {
        // The per-user LaunchAgent written by `portzero setup`
        // (`install_tray_agent` in cli/src/setup.rs).
        std::env::var_os("HOME").is_some_and(|home| {
            Path::new(&home)
                .join("Library/LaunchAgents/cloud.portzero.tray.plist")
                .exists()
        })
    }
    #[cfg(target_os = "windows")]
    {
        // The MSI writes the `PortZeroTray` HKLM `...\CurrentVersion\Run` value
        // from the same component that installs the binary (see
        // `packaging/windows/portzero.wxs`), so the binary being present is
        // equivalent to autostart being registered, without a registry read.
        portzero_domain::app::tray_bin().exists()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

/// Catch "the tray is supposed to be running and there is no tray icon".
///
/// This is the backstop for a whole class of failure that is otherwise
/// completely silent: the tray's single-instance guard declines to start a
/// second tray and exits 0, and a login-time autostart has nowhere to show that
/// message. A user sees no icon, and every other surface — `portzero version`,
/// the app's Version panel, the rest of `doctor` — happily agreed the tray was
/// running, because they all trusted a recorded PID. Nothing pointed at the
/// problem. Now `doctor` looks for the process itself and says so.
pub(super) fn check_tray_running() -> Option<Diagnostic> {
    if tray_presence() != TrayPresence::Missing {
        return None;
    }

    tracing::debug!("check_tray_running: tray autostart installed but no tray process found");
    Some(Diagnostic {
        id: "tray_not_running".into(),
        severity: Severity::Warning,
        category: "system".into(),
        title: "PortZero tray is set to start at login but is not running".to_string(),
        detail: tray_missing_detail(),
        fix: Some(Fix {
            kind: FixKind::Confirm,
            description: TRAY_MISSING_FIX.to_string(),
            command: Some(tray_start_command()),
        }),
    })
}

/// Why there is no tray icon, and what usually causes it. Shared with
/// `portzero doctor` so both surfaces explain it the same way.
pub fn tray_missing_detail() -> String {
    format!(
        "no `portzero-tray` process is running, so there is no PortZero icon in the system \
         tray. Everything else keeps working — the daemon, tunnels, and the desktop app do \
         not depend on the tray. Common causes: the tray was quit for this session; the \
         desktop environment has no system-tray support (some GNOME setups need the \
         AppIndicator extension); or a previous tray exited without cleaning up {} and left \
         it naming a PID that has since been reused.",
        tray_pid_file_display()
    )
}

pub const TRAY_MISSING_FIX: &str =
    "start the tray now (it will also start at your next login); if it exits immediately, run \
     it in a terminal to see why";

/// Path of the tray's single-instance lock, for the diagnostic text.
fn tray_pid_file_display() -> String {
    crate::discovery_loop::DaemonConfig::load()
        .state_dir
        .parent()
        .map(|p| p.join("tray.pid").display().to_string())
        .unwrap_or_else(|| "~/.portzero/tray.pid".to_string())
}

pub fn tray_start_command() -> String {
    let bin = portzero_domain::app::tray_bin();
    let bin = bin.display();
    #[cfg(windows)]
    {
        format!("\"{bin}\"")
    }
    #[cfg(not(windows))]
    {
        // Detached, so the tray outlives the shell that ran the fix.
        format!("nohup \"{bin}\" >/dev/null 2>&1 &")
    }
}
