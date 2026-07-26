//! The muda / tray-icon controller used on Windows and macOS: owns the native
//! tray icon and translates menu clicks and periodic refreshes into state reads
//! and actions. The platform loop (winit) constructs one of these on its
//! event-loop thread and drives it. Action dispatch and autostart live in
//! [`crate::engine`]; the menu structure lives in [`crate::menu`]. Linux uses
//! ksni instead and never compiles this module (it would pull in GTK via muda).

use anyhow::{Context, Result};
use portzero_daemon::discovery_loop::DaemonConfig;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::engine::{self, Dispatch};
use crate::icon;
use crate::menu::{self, MenuModel, MenuSpec};
use crate::state::Snapshot;

pub struct Controller {
    config: DaemonConfig,
    tray: TrayIcon,
    model: MenuModel,
    /// The spec the menu currently installed on the tray was rendered from.
    ///
    /// Kept so [`Controller::refresh`] can install a new menu *only* when the
    /// content differs. Handing the platform a menu — even an identical one —
    /// dismisses whatever the user has open, so an unconditional rebuild every
    /// tick made the menu close itself a few seconds after every click.
    spec: MenuSpec,
    /// Whether we've already tried to auto-launch a stopped daemon once this
    /// session, so we don't fight a user who deliberately stopped it.
    auto_started: bool,
}

/// Build the native tray icon for a snapshot's health.
fn health_icon(snapshot: &Snapshot) -> Result<Icon> {
    let img = icon::image_for_health(snapshot.health);
    Icon::from_rgba(img.rgba, img.width, img.height).context("invalid tray icon buffer")
}

impl Controller {
    /// Build the tray icon from the current daemon state. Must be called on the
    /// event-loop thread (a `tray-icon` requirement on every platform).
    pub fn new() -> Result<Self> {
        let config = DaemonConfig::load();
        let snapshot = Snapshot::read(&config);
        let spec = menu::build(&snapshot);
        let (menu, model) = menu::to_muda(&spec);

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(&snapshot.summary)
            .with_icon(health_icon(&snapshot)?)
            .build()
            .context("failed to create the system tray icon")?;

        Ok(Self {
            config,
            tray,
            model,
            spec,
            auto_started: false,
        })
    }

    /// If the daemon is not running and we haven't already tried once, launch it.
    pub fn maybe_autostart_daemon(&mut self) {
        engine::maybe_autostart(&self.config, &mut self.auto_started);
    }

    /// First run after install: nudge the user to the dashboard, once.
    pub fn maybe_notify_first_run(&self) {
        crate::welcome::maybe_notify_first_run(&self.config);
    }

    /// Re-read daemon state and update the icon, tooltip, and menu.
    ///
    /// The menu is only reinstalled when its content actually changed. Setting
    /// the icon and tooltip is invisible to an open menu, but setting the menu
    /// closes it — so replacing an unchanged menu on every tick meant a user who
    /// opened the tray had it shut in their face one refresh interval later.
    /// A snapshot is derived entirely from on-disk daemon state and carries no
    /// clock, so on a settled machine the rebuilt spec compares equal and the
    /// menu is left alone indefinitely.
    pub fn refresh(&mut self) {
        // Reload the config each tick so an HTTPS toggle we (or the CLI) wrote to
        // config.toml is reflected, and the state dir stays authoritative.
        self.config = DaemonConfig::load();
        let snapshot = Snapshot::read(&self.config);

        match health_icon(&snapshot) {
            Ok(ic) => {
                if let Err(e) = self.tray.set_icon(Some(ic)) {
                    tracing::debug!("failed to update tray icon: {e:#}");
                }
            }
            Err(e) => tracing::debug!("failed to build tray icon: {e:#}"),
        }
        let _ = self.tray.set_tooltip(Some(&snapshot.summary));

        let spec = menu::build(&snapshot);
        if spec == self.spec {
            return;
        }
        let (new_menu, new_model) = menu::to_muda(&spec);
        self.tray.set_menu(Some(Box::new(new_menu)));
        self.model = new_model;
        self.spec = spec;
    }

    /// Handle a menu click by its item id. Returns whether the loop should quit.
    pub fn handle_menu(&mut self, id: &str) -> Dispatch {
        let Some(action) = self.model.get(id).cloned() else {
            return Dispatch::Continue;
        };

        if engine::apply(&self.config, &action) == Dispatch::Quit {
            return Dispatch::Quit;
        }

        // Reflect the new state immediately. Daemon start/stop take a moment to
        // settle in the pid file; the periodic refresh catches the final state.
        self.refresh();
        Dispatch::Continue
    }
}
