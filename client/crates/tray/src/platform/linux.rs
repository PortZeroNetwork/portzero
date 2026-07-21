//! Linux backend: drive the tray from a pure-Rust ksni StatusNotifierItem
//! service (SNI over D-Bus via zbus). Unlike libappindicator/GTK, this links no
//! C GUI toolkit, so the tray adds no `libgtk-3` / `libayatana-appindicator`
//! runtime dependency to the Linux packages. SNI is native on KDE Plasma and
//! works on GNOME with the AppIndicator extension (same as the old backend).
//!
//! ksni delivers menu clicks as closures against the tray object (which lives on
//! a background service thread), so this backend dispatches [`Action`]s directly
//! rather than through the id → action map the muda backend uses. The main
//! thread runs the periodic refresh and watches a shared quit flag.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
use portzero_daemon::discovery_loop::DaemonConfig;

use crate::engine::{self, Dispatch, REFRESH_INTERVAL};
use crate::icon::{self, RgbaImage};
use crate::menu::{self, Action, Node};
use crate::state::Snapshot;

/// How often the main loop wakes to check the quit flag; smaller than
/// [`REFRESH_INTERVAL`] so a Quit click is responsive.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

struct PortzeroTray {
    config: DaemonConfig,
    snapshot: Snapshot,
    /// Set by the Quit menu item; polled by the main loop to exit.
    quit: Arc<AtomicBool>,
}

impl PortzeroTray {
    /// Perform a menu action and re-read state so the next `menu()` render (which
    /// ksni triggers after each activation) reflects it.
    fn on_action(&mut self, action: Action) {
        if engine::apply(&self.config, &action) == Dispatch::Quit {
            self.quit.store(true, Ordering::SeqCst);
        }
        self.reload();
    }

    /// Re-read config and daemon state.
    fn reload(&mut self) {
        self.config = DaemonConfig::load();
        self.snapshot = Snapshot::read(&self.config);
    }
}

impl ksni::Tray for PortzeroTray {
    /// Show the menu on left-click instead of firing [`Self::activate`], matching
    /// the Windows/macOS backend (tray-icon shows its menu on left-click). This
    /// sets the SNI `ItemIsMenu` property so hosts open the menu directly; a
    /// browser tab opening on a plain left-click was surprising behaviour.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "cloud.portzero.tray".into()
    }

    /// Shown as the item's accessible name and, on most SNI hosts, its hover
    /// text — so we surface the one-line status summary here.
    fn title(&self) -> String {
        self.snapshot.summary.clone()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![to_ksni_icon(icon::image_for_health(self.snapshot.health))]
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        to_ksni_items(&menu::build(&self.snapshot).nodes)
    }
}

/// Convert our RGBA (R,G,B,A) status dot to the ARGB32 network-byte-order
/// (A,R,G,B) pixmap the StatusNotifierItem spec requires.
fn to_ksni_icon(img: RgbaImage) -> ksni::Icon {
    let mut data = img.rgba;
    for px in data.chunks_exact_mut(4) {
        let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
        px[0] = a;
        px[1] = r;
        px[2] = g;
        px[3] = b;
    }
    ksni::Icon {
        width: img.width as i32,
        height: img.height as i32,
        data,
    }
}

/// Render the neutral menu tree into ksni menu items, wiring each actionable
/// node to a closure that dispatches its [`Action`] against the tray.
fn to_ksni_items(nodes: &[Node]) -> Vec<MenuItem<PortzeroTray>> {
    nodes
        .iter()
        .map(|node| match node {
            Node::Label(text) => StandardItem {
                label: text.clone(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            Node::Separator => MenuItem::Separator,
            Node::Item {
                label,
                enabled,
                action,
                ..
            } => {
                let action = action.clone();
                StandardItem {
                    label: label.clone(),
                    enabled: *enabled,
                    activate: Box::new(move |t: &mut PortzeroTray| t.on_action(action.clone())),
                    ..Default::default()
                }
                .into()
            }
            Node::Check {
                label,
                checked,
                action,
                ..
            } => {
                let action = action.clone();
                CheckmarkItem {
                    label: label.clone(),
                    checked: *checked,
                    enabled: true,
                    activate: Box::new(move |t: &mut PortzeroTray| t.on_action(action.clone())),
                    ..Default::default()
                }
                .into()
            }
            Node::Sub { label, children } => SubMenu {
                label: label.clone(),
                submenu: to_ksni_items(children),
                ..Default::default()
            }
            .into(),
        })
        .collect()
}

pub fn run() -> Result<()> {
    let config = DaemonConfig::load();
    let snapshot = Snapshot::read(&config);
    let quit = Arc::new(AtomicBool::new(false));

    // Launch the daemon once if it isn't running, mirroring the other backends.
    let mut auto_started = false;
    engine::maybe_autostart(&config, &mut auto_started);

    // First run after install: nudge the user to the dashboard, once.
    crate::welcome::maybe_notify_first_run(&config);

    let tray = PortzeroTray {
        config,
        snapshot,
        quit: quit.clone(),
    };
    let handle = tray
        .spawn()
        .context("failed to register the system tray on the session bus")?;

    // Periodic refresh + quit watch. ksni re-renders after each menu activation
    // on its own, so this loop only needs to pick up out-of-band state changes
    // (the CLI starting/stopping the daemon, new tunnels) and honour Quit.
    let mut since_refresh = Duration::ZERO;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        if quit.load(Ordering::SeqCst) || handle.is_closed() {
            break;
        }
        since_refresh += POLL_INTERVAL;
        if since_refresh >= REFRESH_INTERVAL {
            since_refresh = Duration::ZERO;
            let _ = handle.update(|t: &mut PortzeroTray| t.reload());
        }
    }

    let _ = handle.shutdown();
    Ok(())
}
