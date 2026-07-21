//! Read/write helpers for `cloud_state.json`, the daemon-to-CLI channel for
//! cloud connection status, plan, and upsell messaging.

use serde::{Deserialize, Serialize};

use super::config::DaemonConfig;

/// Serializable cloud connection state persisted to cloud_state.json.
/// Extended to carry plan (for upsell prompts) and a user-facing status_message
/// (e.g. when the edge rejects a cloud route due to PlanLimitExceeded).
#[derive(Serialize, Deserialize, Default, Clone)]
struct CloudStateFile {
    connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    can_use_cloud_tunnels: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// Write the current cloud connection state (and plan / user message) to disk.
pub(super) fn write_cloud_state(
    config: &DaemonConfig,
    connected: bool,
    error: Option<String>,
    plan: Option<String>,
    can_use_cloud_tunnels: Option<bool>,
    message: Option<String>,
) {
    let path = config.cloud_state_path();
    let state = CloudStateFile {
        connected,
        plan,
        can_use_cloud_tunnels,
        error,
        message,
    };
    match serde_json::to_string(&state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("Failed to write cloud state: {}", e);
            }
        }
        Err(e) => tracing::warn!("Failed to serialize cloud state: {}", e),
    }
}

/// Read the full persisted cloud state (internal helper).
fn read_full_cloud_state(config: &DaemonConfig) -> CloudStateFile {
    read_full_cloud_state_from_path(&config.cloud_state_path())
}

fn read_full_cloud_state_from_path(p: &std::path::Path) -> CloudStateFile {
    let content = match std::fs::read_to_string(p) {
        Ok(c) => c,
        Err(_) => return CloudStateFile::default(),
    };
    serde_json::from_str(&content).unwrap_or_default()
}

/// Read the cloud connection state written by the running daemon.
/// Returns None if the file doesn't exist or can't be parsed.
pub fn read_cloud_connected(config: &DaemonConfig) -> Option<bool> {
    read_cloud_connected_from_path(&config.cloud_state_path())
}

fn read_cloud_connected_from_path(path: &std::path::Path) -> Option<bool> {
    let content = std::fs::read_to_string(path).ok()?;
    if content.contains("\"connected\":true") {
        Some(true)
    } else if content.contains("\"connected\":false") {
        Some(false)
    } else {
        None
    }
}

/// Read the last connection error stored by the running daemon, if any.
pub fn read_cloud_error(config: &DaemonConfig) -> Option<String> {
    read_full_cloud_state(config).error
}

/// Read the plan reported by the edge (e.g. "free", "pro").
pub fn read_cloud_plan(config: &DaemonConfig) -> Option<String> {
    read_full_cloud_state(config).plan
}

/// Convenience path-based reader (used by diagnostics which only has state_dir).
pub fn read_cloud_plan_from_path(state_dir: &std::path::Path) -> Option<String> {
    read_full_cloud_state_from_path(&state_dir.join("cloud_state.json")).plan
}

/// Read whether the account can create cloud tunnels right now (own plan or
/// a paid team it belongs to), as last reported by the edge on Welcome.
pub fn read_cloud_can_use_tunnels(config: &DaemonConfig) -> Option<bool> {
    read_full_cloud_state(config).can_use_cloud_tunnels
}

/// Convenience path-based reader (used by diagnostics which only has state_dir).
pub fn read_cloud_can_use_tunnels_from_path(state_dir: &std::path::Path) -> Option<bool> {
    read_full_cloud_state_from_path(&state_dir.join("cloud_state.json")).can_use_cloud_tunnels
}

/// Read the latest user-facing status message (plan limits, upsell, etc.).
pub fn read_cloud_message(config: &DaemonConfig) -> Option<String> {
    read_full_cloud_state(config).message
}

/// Convenience path-based reader.
pub fn read_cloud_message_from_path(state_dir: &std::path::Path) -> Option<String> {
    read_full_cloud_state_from_path(&state_dir.join("cloud_state.json")).message
}
