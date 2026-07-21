//! Autostart control commands: enable, disable, status.
//!
//! These wrap `portzero_daemon::autostart`, which installs the daemon as a
//! native system service (macOS root LaunchDaemon, Linux systemd user unit,
//! Windows scheduled task). The underlying functions are cfg-gated per platform,
//! so the same three subcommands work everywhere.

use anyhow::Result;

use portzero_daemon::autostart::{install_autostart, is_autostart_installed, uninstall_autostart};

/// Enable autostart: install the daemon as a system service that starts at boot.
pub fn enable() -> Result<()> {
    // On macOS this bails with an actionable sudo hint when not run as root.
    install_autostart()?;

    println!("Autostart enabled.");
    println!("The daemon will now start automatically at boot.");
    if let Some(loc) = service_location() {
        println!("Service installed at: {loc}");
    }
    Ok(())
}

/// Disable autostart: remove the installed system service.
pub fn disable() -> Result<()> {
    // No-op (and Ok) if not currently installed; macOS still requires root.
    uninstall_autostart()?;

    println!("Autostart disabled.");
    println!("The daemon will no longer start automatically at boot.");
    Ok(())
}

/// Report whether autostart is currently installed.
pub fn status() -> Result<()> {
    if is_autostart_installed() {
        println!("Autostart: installed");
        if let Some(loc) = service_location() {
            println!("Service:   {loc}");
        }
        println!("The daemon will start automatically at boot.");
    } else {
        println!("Autostart: not installed");
        println!("Run `portzero autostart enable` to start the daemon at boot.");
    }
    Ok(())
}

/// Best-effort, human-readable location of the platform service definition.
/// Returns `None` on unsupported platforms.
fn service_location() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        Some("/Library/LaunchDaemons/cloud.portzero.daemon.plist (root LaunchDaemon)".to_string())
    }

    #[cfg(target_os = "linux")]
    {
        dirs::home_dir().map(|home| {
            home.join(".config/systemd/user/portzero-daemon.service")
                .display()
                .to_string()
        })
    }

    #[cfg(target_os = "windows")]
    {
        Some("scheduled task \"cloud.portzero.daemon\" (start at logon)".to_string())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}
