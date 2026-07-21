//! Platform-neutral tray engine: the action-dispatch and daemon-autostart logic
//! shared by every backend. It holds no tray handle and knows nothing about
//! muda, tray-icon, or ksni — the platform layers own the native tray and call
//! into here so a menu click behaves identically on macOS, Windows, and Linux.

use std::time::Duration;

use anyhow::Result;
use portzero_daemon::discovery_loop::DaemonConfig;

use crate::actions;
use crate::menu::Action;
use crate::state::Snapshot;

/// How often the tray re-reads daemon state and rebuilds its menu. State reads
/// are just a handful of small file reads, so this is cheap to run often.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// What the platform loop should do after handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// Keep running.
    Continue,
    /// The user asked to quit the tray.
    Quit,
}

/// Execute a menu [`Action`] against the current config, returning whether the
/// tray should quit. This performs the side effect (start/stop the daemon, open
/// a URL, flip the HTTPS policy) but never touches a native tray handle, so both
/// the muda and the ksni backends dispatch through it unchanged.
pub fn apply(config: &DaemonConfig, action: &Action) -> Dispatch {
    match action {
        Action::Quit => return Dispatch::Quit,
        Action::Refresh => {}
        Action::Start => log_err("start daemon", actions::start_daemon()),
        Action::Stop => log_err("stop daemon", actions::stop_daemon()),
        Action::Restart => log_err("restart daemon", actions::restart_daemon()),
        Action::OpenApp => log_err("open app", actions::open_app()),
        Action::OpenUrl(url) => log_err("open url", actions::open_url(url)),
        Action::ToggleHttps(enabled) => log_err(
            "set https policy",
            actions::set_https_enabled(config, *enabled),
        ),
    }
    Dispatch::Continue
}

/// If the daemon is not running and we haven't already tried once this session,
/// launch it (`portzero start`). This delivers the "install it and it just
/// works" behaviour: the tray comes up, notices the daemon is down, and starts
/// it — without fighting a user who deliberately stopped it later.
pub fn maybe_autostart(config: &DaemonConfig, auto_started: &mut bool) {
    if *auto_started {
        return;
    }
    let snapshot = Snapshot::read(config);
    if !snapshot.running {
        *auto_started = true;
        tracing::info!("daemon is not running; launching `portzero start`");
        if let Err(e) = actions::start_daemon() {
            tracing::warn!("failed to auto-start daemon: {e:#}");
        }
    }
}

fn log_err(what: &str, result: Result<()>) {
    if let Err(e) = result {
        tracing::warn!("{what} failed: {e:#}");
    }
}
