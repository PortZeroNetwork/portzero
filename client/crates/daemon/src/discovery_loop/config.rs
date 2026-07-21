//! Daemon configuration: the persisted `config.toml` schema and the effective
//! `DaemonConfig` the loop runs with.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::net::dns::DnsFirstHitPolicy;
use crate::net::stack::OverlayHttpsPolicy;

/// Configuration for the discovery daemon.
#[derive(Clone)]
pub struct DaemonConfig {
    /// Directory for daemon state files (routes.json, daemon.pid, daemon.log).
    pub state_dir: PathBuf,
    /// How often to scan for services, in seconds.
    pub scan_interval_secs: u64,
    /// HTTPS behavior for `.portzero.local` overlay services.
    pub overlay_https: OverlayHttpsPolicy,
    /// How DNS handles first hits for unknown `.portzero.local` services.
    pub dns_first_hit_policy: DnsFirstHitPolicy,
    /// When a new HTTP/HTTPS local tunnel is detected (a `.portzero.local`
    /// backend on port 80 or 443), open its URL in the default browser once.
    /// On by default so a freshly-started example pops a browser tab.
    pub auto_open_http_tunnels: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let state_dir = dirs::home_dir()
            .map(|h| h.join(".portzero").join("daemon"))
            .unwrap_or_else(|| PathBuf::from(".portzero/daemon"));

        Self {
            state_dir,
            scan_interval_secs: 2,
            overlay_https: OverlayHttpsPolicy::default(),
            dns_first_hit_policy: DnsFirstHitPolicy::default(),
            auto_open_http_tunnels: true,
        }
    }
}

impl DaemonConfig {
    /// Load daemon configuration from `~/.portzero/config.toml`, falling back to
    /// defaults when the file is absent or invalid.
    pub fn load() -> Self {
        let mut config = Self::default();
        let path = config.config_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return config;
        };

        match toml::from_str::<FileConfig>(&raw) {
            Ok(file) => file.apply_to(&mut config),
            Err(e) => tracing::warn!("Ignoring invalid daemon config {}: {e}", path.display()),
        }
        config
    }

    /// Path to the user-editable TOML config file.
    pub fn config_path(&self) -> PathBuf {
        self.state_dir
            .parent()
            .map(|p| p.join("config.toml"))
            .unwrap_or_else(|| PathBuf::from(".portzero/config.toml"))
    }

    /// Path to the routes file.
    pub fn routes_path(&self) -> PathBuf {
        self.state_dir.join("routes.json")
    }

    /// Path to the PID file.
    pub fn pid_path(&self) -> PathBuf {
        self.state_dir.join("daemon.pid")
    }

    /// Path to the log file.
    pub fn log_path(&self) -> PathBuf {
        self.state_dir.join("daemon.log")
    }

    /// Path to the cloud connection state file.
    pub fn cloud_state_path(&self) -> PathBuf {
        self.state_dir.join("cloud_state.json")
    }

    /// Path to the per-cloud-route review status file (domain → status),
    /// written by the cloud connector and read by the local dashboard.
    pub fn cloud_route_status_path(&self) -> PathBuf {
        self.state_dir.join("cloud_route_status.json")
    }

    /// Path to the visibility "issues" state file (duplicate names, etc.).
    pub fn issues_path(&self) -> PathBuf {
        self.state_dir.join("issues.json")
    }

    /// Path to the overlay services state file (read by `portzero status`).
    pub fn overlay_path(&self) -> PathBuf {
        self.state_dir.join("overlay.json")
    }

    /// Path to the auto-open tracker state file (which web tunnels we have
    /// already popped a browser tab for), so a daemon restart does not
    /// reopen tabs for tunnels that were already running.
    pub fn auto_open_path(&self) -> PathBuf {
        self.state_dir.join("auto_open.json")
    }

    /// Path to the observed runtime-truth file (observed edges + exercised
    /// routes), read by `portzero inspect` and the MCP server.
    pub fn observations_path(&self) -> PathBuf {
        self.state_dir.join("observations.json")
    }

    /// Path to the diagnostics report file.
    pub fn diagnostics_path(&self) -> PathBuf {
        self.state_dir.join("diagnostics.json")
    }

    /// Write (or update) only the HTTPS policy section in the config file (next to state dir,
    /// typically `~/.portzero/config.toml`). Preserves other existing keys/sections using
    /// a TOML value merge.
    pub fn write_https_policy(&self, policy: OverlayHttpsPolicy) -> Result<()> {
        let path = self.config_path();

        let mut root: toml::Value = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(raw) => {
                    toml::from_str(&raw).unwrap_or_else(|_| toml::Value::Table(Default::default()))
                }
                Err(_) => toml::Value::Table(Default::default()),
            }
        } else {
            toml::Value::Table(Default::default())
        };

        // Ensure [overlay.https] table exists and set the three keys.
        if let Some(tbl) = root.as_table_mut() {
            let overlay = tbl
                .entry("overlay".to_owned())
                .or_insert(toml::Value::Table(Default::default()));
            if let Some(ov) = overlay.as_table_mut() {
                let https = ov
                    .entry("https".to_owned())
                    .or_insert(toml::Value::Table(Default::default()));
                if let Some(h) = https.as_table_mut() {
                    h.insert(
                        "enable_for_port_80".to_owned(),
                        toml::Value::Boolean(policy.enable_for_port_80),
                    );
                    h.insert(
                        "redirect_port_80".to_owned(),
                        toml::Value::Boolean(policy.redirect_port_80),
                    );
                    h.insert(
                        "passthrough_port_443".to_owned(),
                        toml::Value::Boolean(policy.passthrough_port_443),
                    );
                }
            }
        }

        let serialized =
            toml::to_string_pretty(&root).context("serializing config.toml for https policy")?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir for {}", path.display()))?;
        }
        std::fs::write(&path, serialized)
            .with_context(|| format!("writing https policy to {}", path.display()))?;
        Ok(())
    }

    /// Write (or update) only the `[daemon] auto_open_http_tunnels` key in the
    /// config file, preserving every other key/section via a TOML value merge.
    /// A running daemon picks the change up on its next config-reload poll.
    pub fn write_auto_open_http_tunnels(&self, enabled: bool) -> Result<()> {
        let path = self.config_path();

        let mut root: toml::Value = match std::fs::read_to_string(&path) {
            Ok(raw) => {
                toml::from_str(&raw).unwrap_or_else(|_| toml::Value::Table(Default::default()))
            }
            Err(_) => toml::Value::Table(Default::default()),
        };

        if let Some(tbl) = root.as_table_mut() {
            let daemon = tbl
                .entry("daemon".to_owned())
                .or_insert(toml::Value::Table(Default::default()));
            if let Some(d) = daemon.as_table_mut() {
                d.insert(
                    "auto_open_http_tunnels".to_owned(),
                    toml::Value::Boolean(enabled),
                );
            }
        }

        let serialized = toml::to_string_pretty(&root)
            .context("serializing config.toml for auto_open_http_tunnels")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir for {}", path.display()))?;
        }
        std::fs::write(&path, serialized)
            .with_context(|| format!("writing auto_open_http_tunnels to {}", path.display()))?;
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct FileConfig {
    daemon: Option<FileDaemonConfig>,
    overlay: Option<FileOverlayConfig>,
}

impl FileConfig {
    pub(super) fn apply_to(self, config: &mut DaemonConfig) {
        if let Some(daemon) = self.daemon {
            if let Some(scan_interval_secs) = daemon.scan_interval_secs {
                config.scan_interval_secs = scan_interval_secs;
            }
            if let Some(auto_open) = daemon.auto_open_http_tunnels {
                config.auto_open_http_tunnels = auto_open;
            }
        }
        if let Some(overlay) = self.overlay {
            overlay.apply_to(config);
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileDaemonConfig {
    scan_interval_secs: Option<u64>,
    auto_open_http_tunnels: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct FileOverlayConfig {
    https: Option<FileOverlayHttpsConfig>,
    dns_first_hit_policy: Option<DnsFirstHitPolicy>,
}

impl FileOverlayConfig {
    fn apply_to(self, config: &mut DaemonConfig) {
        if let Some(policy) = self.dns_first_hit_policy {
            config.dns_first_hit_policy = policy;
        }
        if let Some(https) = self.https {
            if let Some(v) = https.enable_for_port_80 {
                config.overlay_https.enable_for_port_80 = v;
            }
            if let Some(v) = https.redirect_port_80 {
                config.overlay_https.redirect_port_80 = v;
            }
            if let Some(v) = https.passthrough_port_443 {
                config.overlay_https.passthrough_port_443 = v;
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileOverlayHttpsConfig {
    enable_for_port_80: Option<bool>,
    redirect_port_80: Option<bool>,
    passthrough_port_443: Option<bool>,
}
