//! First-run welcome nudge: pop a desktop notification pointing the user at the
//! PortZero desktop app the first time the tray starts after install.
//!
//! There is no cross-platform notification crate in our dependency set (and we
//! deliberately avoid GUI-toolkit deps — see the crate docs), so this shells out
//! to each platform's native notifier, best-effort. A marker file under
//! `~/.portzero/` makes it fire once, not on every login.

use std::path::PathBuf;
use std::process::Command;

use portzero_daemon::discovery_loop::DaemonConfig;

const TITLE: &str = "PortZero is running";
const BODY: &str = "Open the PortZero app to get started and run an example.";

/// Show the welcome notification once. Subsequent calls (later logins) no-op.
pub fn maybe_notify_first_run(config: &DaemonConfig) {
    let marker = marker_path(config);
    if marker.exists() {
        return;
    }
    // Write the marker before notifying so a notifier that errors out doesn't
    // leave us nagging on every launch.
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&marker, b"welcomed\n").is_err() {
        // Couldn't persist the marker — skip rather than risk notifying forever.
        return;
    }
    notify(TITLE, BODY);
}

/// `~/.portzero/tray-welcomed` (sits next to the daemon state dir).
fn marker_path(config: &DaemonConfig) -> PathBuf {
    config
        .state_dir
        .parent()
        .map(|p| p.join("tray-welcomed"))
        .unwrap_or_else(|| PathBuf::from(".portzero/tray-welcomed"))
}

#[cfg(target_os = "linux")]
fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["-a", "PortZero", "-i", "network-server", title, body])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(target_os = "macos")]
fn notify(title: &str, body: &str) {
    // AppleScript strings use double quotes; swap any out of the text.
    let body = body.replace('"', "'");
    let title = title.replace('"', "'");
    let script = format!("display notification \"{body}\" with title \"{title}\"");
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(target_os = "windows")]
fn notify(title: &str, body: &str) {
    // Best-effort Win10+ toast via the WinRT notification API. Single-quote any
    // quotes so the embedded PowerShell string literals stay well-formed.
    let title = title.replace('\'', "`'");
    let body = body.replace('\'', "`'");
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         [Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]|Out-Null;\
         $t=[Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02);\
         $x=$t.GetElementsByTagName('text');\
         $x.Item(0).AppendChild($t.CreateTextNode('{title}'))|Out-Null;\
         $x.Item(1).AppendChild($t.CreateTextNode('{body}'))|Out-Null;\
         $toast=[Windows.UI.Notifications.ToastNotification]::new($t);\
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('PortZero').Show($toast);"
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn notify(_title: &str, _body: &str) {}
