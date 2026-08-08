//! PortZero system-tray companion library.
//!
//! A small, opt-in GUI companion (deliberately separate from the headless
//! daemon) that gives the daemon a face in the menu bar / system tray:
//!
//! - A green / amber / red status dot reflecting daemon and tunnel health, read
//!   entirely from the daemon's on-disk state so it works even when the daemon
//!   is **down** — the case a new user is most likely to hit.
//! - If the daemon isn't running when the tray starts, it launches it once
//!   (`portzero start`), and offers Start / Restart / Stop from the menu.
//! - A browsable list of local and cloud tunnels (with their http/https scheme),
//!   a global "enable HTTPS for HTTP tunnels" toggle, and any current issues.
//!
//! The menu is built once in [`menu`] and used unchanged on every platform, so
//! it stays identical across macOS, Windows, and Linux.

use portzero_daemon::discovery_loop::DaemonConfig;
use portzero_daemon::versions;

pub mod actions;
// The muda/tray-icon controller is Windows/macOS only; Linux drives ksni
// directly from `platform::linux` and never compiles muda (which links GTK).
#[cfg(not(target_os = "linux"))]
pub mod controller;
pub mod engine;
pub mod icon;
pub mod menu;
pub mod platform;
mod singleton;
pub mod state;
pub mod welcome;

/// Entry point used by the `portzero-tray` binary.
///
/// Refuses to start a second tray if one is already running ([`singleton`]) —
/// running two would register two tray icons for the same daemon. Otherwise
/// falls through to the platform-specific event loop.
pub fn run() -> anyhow::Result<()> {
    if let Err(existing_pid) = singleton::acquire() {
        // Also on stderr, not just the log: someone who ran `portzero-tray`
        // from a terminal because no icon appeared needs to be told what
        // happened and how to check. A bare exit 0 reads as "it worked", which
        // is the least useful thing this can say.
        eprintln!(
            "portzero-tray is already running as PID {existing_pid}, so this instance exited \
             rather than register a second tray icon.\n\
             If you cannot see a PortZero icon in your system tray:\n\
             - Confirm that process is really a tray: `portzero doctor`\n\
             - Your desktop may not show tray icons by default (some GNOME setups need the \
               AppIndicator extension).\n\
             - To force a fresh tray: `kill {existing_pid}` and start portzero-tray again."
        );
        tracing::info!(
            "portzero-tray (PID {existing_pid}) is already running; not starting a duplicate tray icon"
        );
        return Ok(());
    }

    // Publish which build this tray is running, so the desktop app's Version
    // panel and `portzero version` can see a tray left over from before an
    // upgrade. Only the tray that won the singleton announces itself.
    let config = DaemonConfig::load();
    versions::announce(&config, versions::Component::Tray, versions::BUILD_VERSION);
    let result = platform::run();
    versions::withdraw(&config, versions::Component::Tray);
    singleton::release();
    result
}
