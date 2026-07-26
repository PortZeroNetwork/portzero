//! Auto-start support: install/uninstall the daemon as a system service.
//!
//! - macOS: root LaunchDaemon plist in /Library/LaunchDaemons/ (runs as root,
//!   so the privileged overlay — utun + /etc/resolver + routes — actually comes
//!   up; install/uninstall therefore require `sudo`).
//! - Linux: systemd user unit in ~/.config/systemd/user/
//! - Windows: scheduled task (start at logon)

use anyhow::{Context, Result};
#[cfg(target_os = "windows")]
use std::path::Path;
use std::path::PathBuf;

/// Service label / unit name used across platforms.
#[allow(dead_code)]
const SERVICE_NAME: &str = "cloud.portzero.daemon";

/// Install auto-start for the daemon.
///
/// After installation, the daemon starts automatically on user login.
pub fn install_autostart() -> Result<()> {
    let binary = find_daemon_binary()?;

    #[cfg(target_os = "macos")]
    install_launchd(&binary)?;

    #[cfg(target_os = "linux")]
    install_systemd(&binary)?;

    #[cfg(target_os = "windows")]
    install_windows_task(&binary)?;

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = binary;
        anyhow::bail!(
            "Auto-start is not supported on this platform.\n\n\
             You can still run the daemon manually with: portzero daemon"
        );
    }

    Ok(())
}

/// Uninstall auto-start for the daemon.
pub fn uninstall_autostart() -> Result<()> {
    #[cfg(target_os = "macos")]
    uninstall_launchd()?;

    #[cfg(target_os = "linux")]
    uninstall_systemd()?;

    #[cfg(target_os = "windows")]
    uninstall_windows_task()?;

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    anyhow::bail!("Auto-start is not supported on this platform.");

    Ok(())
}

/// Check whether auto-start is installed.
pub fn is_autostart_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        launchd_plist_path().exists()
    }

    #[cfg(target_os = "linux")]
    {
        systemd_unit_path().exists()
    }

    #[cfg(target_os = "windows")]
    {
        is_windows_task_installed()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    false
}

/// Find the path to the portzero binary.
fn find_daemon_binary() -> Result<PathBuf> {
    // First: check if we're running as the binary ourselves
    let current_exe =
        std::env::current_exe().context("Failed to determine current executable path")?;

    // If the current exe looks like portzero, use it
    if let Some(name) = current_exe.file_name() {
        let name_str = name.to_string_lossy();
        if name_str.starts_with("portzero") {
            return Ok(current_exe);
        }
    }

    // Otherwise, search PATH
    if let Ok(path) = which("portzero") {
        return Ok(path);
    }

    anyhow::bail!(
        "Could not find portzero binary.\n\n\
         Ensure portzero is installed and on your PATH, then retry.\n\
         Install with: curl -fsSL https://portzero.net/install.sh | sh"
    )
}

/// Simple which(1) equivalent.
fn which(name: &str) -> Result<PathBuf> {
    let Some(path_env) = std::env::var_os("PATH") else {
        anyhow::bail!("{name} not found in PATH");
    };

    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("{} not found in PATH", name)
}

/// System LaunchDaemon directory. The daemon must run as **root** so the
/// privileged overlay (utun + `/etc/resolver` + routes) can come up, so the
/// plist lives in the system domain rather than the per-user LaunchAgents dir.
#[cfg(target_os = "macos")]
const LAUNCHDAEMONS_DIR: &str = "/Library/LaunchDaemons";

/// Root-writable log directory. A LaunchDaemon runs as root, whose home is not
/// the installing user's, so logs cannot live under `~/.portzero`.
#[cfg(target_os = "macos")]
const DAEMON_LOG_DIR: &str = "/Library/Logs/portzero";

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> PathBuf {
    PathBuf::from(LAUNCHDAEMONS_DIR).join(format!("{}.plist", SERVICE_NAME))
}

/// Build the LaunchDaemon plist XML. Pure and unit-testable (no I/O), so the
/// generated content can be asserted without root.
///
/// `home_dir` — when set, an `EnvironmentVariables` block pins `HOME` to the
/// real user's home directory.  Without this the daemon (running as root) uses
/// `/var/root`, so its state files land in a different directory than the one
/// the user-mode CLI reads — `portzero status` and `portzero stop` break.
#[cfg(target_os = "macos")]
fn render_launchd_plist(binary: &str, log_dir: &str, home_dir: Option<&str>) -> String {
    let env_block = match home_dir {
        Some(home) => format!(
            "    <key>EnvironmentVariables</key>\n\
             <dict>\n\
                 <key>HOME</key>\n\
                 <string>{home}</string>\n\
             </dict>\n"
        ),
        None => String::new(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>start</string>
        <string>--foreground</string>
    </array>
    {env_block}<key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log_dir}/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/daemon.log</string>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        label = SERVICE_NAME,
        binary = binary,
        log_dir = log_dir,
        env_block = env_block,
    )
}

/// Detect the home directory of the real (non-root) user when this process was
/// launched via `sudo`.  `sudo` sets `SUDO_USER` to the invoking user's login
/// name; we resolve their home from `/etc/passwd` so the LaunchDaemon plist
/// can pin `HOME` to that path even though we're running as root.
#[cfg(target_os = "macos")]
fn real_user_home() -> Option<String> {
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if !sudo_user.is_empty() && sudo_user != "root" {
            if let Some(home) = macos_passwd_home(&sudo_user) {
                return Some(home);
            }
        }
    }
    // Fall back to the HOME environment variable (may already be the real
    // user's home if sudo was invoked with `-E`, or is `/var/root` otherwise).
    std::env::var("HOME").ok()
}

/// Parse `/etc/passwd` to find the home directory for `username`.
#[cfg(target_os = "macos")]
fn macos_passwd_home(username: &str) -> Option<String> {
    let content = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in content.lines() {
        let mut fields = line.splitn(7, ':');
        let name = fields.next()?;
        if name != username {
            continue;
        }
        // Fields: name:pw:uid:gid:gecos:home:shell — home is index 5, i.e. nth(4)
        let home = fields.nth(4)?;
        return Some(home.to_string());
    }
    None
}

/// Fail with an actionable message if we are not root. Writing to
/// `/Library/LaunchDaemons` and bootstrapping the system domain both require it.
#[cfg(target_os = "macos")]
fn require_root(action: &str) -> Result<()> {
    // SAFETY: geteuid is always safe to call.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        anyhow::bail!(
            "Installing the port-zero autostart service as a root LaunchDaemon \
             requires administrator privileges to {action} {dir}.\n\n\
             Re-run this command with sudo, e.g.:\n    \
             sudo portzero autostart {verb}",
            action = action,
            dir = LAUNCHDAEMONS_DIR,
            verb = if action.contains("remove") {
                "disable"
            } else {
                "enable"
            },
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_launchd(binary: &std::path::Path) -> Result<()> {
    require_root("write to")?;

    let plist_path = launchd_plist_path();
    let plist_dir = plist_path.parent().expect(
        "launchd plist path has no parent directory — this is a bug in launchd_plist_path()",
    );
    std::fs::create_dir_all(plist_dir)
        .with_context(|| format!("Failed to create {}", plist_dir.display()))?;

    let log_dir = PathBuf::from(DAEMON_LOG_DIR);
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("Failed to create log directory {}", log_dir.display()))?;

    let home_dir = real_user_home();
    let plist = render_launchd_plist(
        &binary.display().to_string(),
        DAEMON_LOG_DIR,
        home_dir.as_deref(),
    );

    std::fs::write(&plist_path, &plist).with_context(|| {
        format!(
            "Failed to write LaunchDaemon plist to {}",
            plist_path.display()
        )
    })?;

    // Reload the job if it already exists so launchd picks up the latest plist.
    bootout_and_wait();
    bootstrap_launchd(&plist_path)?;

    kickstart_launchd()?;
    verify_launchd_loaded()?;

    tracing::info!("Installed LaunchDaemon: {}", plist_path.display());
    Ok(())
}

/// How long to wait for launchd to finish an asynchronous domain operation.
#[cfg(target_os = "macos")]
const LAUNCHD_SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Run `launchctl` with `args`, capturing its output.
#[cfg(target_os = "macos")]
fn launchctl(args: &[&std::ffi::OsStr]) -> Result<std::process::Output> {
    std::process::Command::new("launchctl")
        .args(args)
        .output()
        .context("Failed to run launchctl. Is /bin/launchctl present and executable?")
}

/// The system-domain service target for the daemon (`system/cloud.portzero.daemon`).
#[cfg(target_os = "macos")]
fn launchd_service_target() -> String {
    format!("system/{}", SERVICE_NAME)
}

/// Whether launchd currently has the daemon loaded in the system domain.
#[cfg(target_os = "macos")]
fn launchd_service_loaded() -> bool {
    use std::ffi::OsStr;

    launchctl(&[OsStr::new("print"), OsStr::new(&launchd_service_target())])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Boot the service out, then wait until launchd has really torn it down.
///
/// `bootout` is asynchronous: it returns once launchd has *accepted* the
/// request, not once the job is gone. Issuing `bootstrap` inside that window
/// fails with "Operation already in progress", and because the old code
/// discarded bootout's result and bootstrapped immediately, that race left the
/// machine in its worst possible state — a valid, enabled plist on disk with no
/// service loaded behind it. `portzero autostart status` and every file-existence
/// check then reported autostart as installed while no privileged daemon existed,
/// and the unprivileged one the tray starts in its place cannot bring the overlay
/// up. Waiting here is what makes the following `bootstrap` deterministic.
#[cfg(target_os = "macos")]
fn bootout_and_wait() {
    use std::ffi::OsStr;

    if !launchd_service_loaded() {
        return;
    }
    let target = launchd_service_target();
    let _ = launchctl(&[OsStr::new("bootout"), OsStr::new(&target)]);

    let deadline = std::time::Instant::now() + LAUNCHD_SETTLE_TIMEOUT;
    while launchd_service_loaded() {
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                "launchd still reports {target} as loaded {}s after bootout; \
                 bootstrapping anyway",
                LAUNCHD_SETTLE_TIMEOUT.as_secs()
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Load the daemon into the system domain, retrying while launchd is still busy.
///
/// Every failure path reports what `launchctl` actually said. The previous
/// message named only the plist path, so the one diagnostic that explains a
/// failed install — launchd's own errno and text — was discarded exactly when
/// it was needed.
#[cfg(target_os = "macos")]
fn bootstrap_launchd(plist_path: &std::path::Path) -> Result<()> {
    use std::ffi::OsStr;

    let deadline = std::time::Instant::now() + LAUNCHD_SETTLE_TIMEOUT;
    let mut last_error;
    loop {
        let out = launchctl(&[
            OsStr::new("bootstrap"),
            OsStr::new("system"),
            plist_path.as_os_str(),
        ])?;
        // Treat "loaded" as success regardless of exit status: bootstrap reports
        // an already-loaded service as an error, which is not one for us.
        if out.status.success() || launchd_service_loaded() {
            return Ok(());
        }
        last_error = describe_launchctl(&out);
        if !launchd_is_busy(&last_error) || std::time::Instant::now() >= deadline {
            break;
        }
        tracing::debug!("launchctl bootstrap is still busy ({last_error}); retrying");
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    // Fall back to the legacy verb so older systems still load the daemon.
    tracing::warn!("launchctl bootstrap failed ({last_error}); trying legacy `load -w`");
    let fallback = launchctl(&[OsStr::new("load"), OsStr::new("-w"), plist_path.as_os_str()])?;
    if fallback.status.success() || launchd_service_loaded() {
        return Ok(());
    }

    anyhow::bail!(
        "launchd would not load the PortZero daemon from {path}.\n\n\
         `launchctl bootstrap system` said: {last_error}\n\
         `launchctl load -w` said: {fallback}\n\n\
         The plist itself is written and valid — only loading it failed, so the \
         daemon will not start at boot and any daemon you start by hand will run \
         without the privileges its overlay network needs.\n\n\
         Things that cause this:\n  \
         - the service is disabled in launchd's override database; re-enable it \
         with `sudo launchctl enable {target}`\n  \
         - a previous copy is still shutting down; wait a few seconds and re-run \
         `sudo portzero autostart enable`\n  \
         - the plist is not owned by root:wheel; fix with `sudo chown root:wheel \
         {path}`\n\n\
         Verify the plist with `plutil -lint {path}` and inspect launchd's view \
         with `sudo launchctl print {target}`.",
        path = plist_path.display(),
        target = launchd_service_target(),
        last_error = last_error,
        fallback = describe_launchctl(&fallback),
    )
}

/// One-line rendering of a failed `launchctl` invocation, for error messages.
#[cfg(target_os = "macos")]
fn describe_launchctl(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let text = format!("{} {}", stderr.trim(), stdout.trim());
    let text = text.trim();
    if text.is_empty() {
        format!("exited with {} and printed nothing", out.status)
    } else {
        text.to_string()
    }
}

/// Whether launchctl failed because another domain operation is still running.
///
/// launchd reports these as errno 36 ("Operation now in progress") and 37
/// ("Operation already in progress"). They are the retryable failures — every
/// other one will fail identically no matter how long we wait.
#[cfg(target_os = "macos")]
fn launchd_is_busy(message: &str) -> bool {
    message.contains("in progress")
}

#[cfg(target_os = "macos")]
fn kickstart_launchd() -> Result<()> {
    let service_target = format!("system/{}", SERVICE_NAME);
    let output = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &service_target])
        .output()
        .context("Failed to run launchctl kickstart")?;

    if !output.status.success() {
        anyhow::bail!(
            "launchctl kickstart failed for {service_target}.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim(),
        );
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_launchd_loaded() -> Result<()> {
    let service_target = format!("system/{}", SERVICE_NAME);
    let output = std::process::Command::new("launchctl")
        .args(["print", &service_target])
        .output()
        .context("Failed to run launchctl print")?;

    if !output.status.success() {
        anyhow::bail!(
            "LaunchDaemon was written, but launchd does not report {service_target} as loaded.\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim(),
        );
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<()> {
    require_root("remove from")?;

    let plist_path = launchd_plist_path();

    if plist_path.exists() {
        // Modern launchctl: `bootout system/<label>` unloads a system daemon
        // (`unload -w` is deprecated for the system domain).
        let status = std::process::Command::new("launchctl")
            .arg("bootout")
            .arg(format!("system/{}", SERVICE_NAME))
            .status();

        if !matches!(status, Ok(s) if s.success()) {
            // Fall back to the legacy verb if bootout isn't available / fails.
            let _ = std::process::Command::new("launchctl")
                .args(["unload", "-w"])
                .arg(&plist_path)
                .status();
        }

        std::fs::remove_file(&plist_path).with_context(|| {
            format!(
                "Failed to remove LaunchDaemon plist at {}",
                plist_path.display()
            )
        })?;

        tracing::info!("Uninstalled LaunchDaemon: {}", plist_path.display());
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config/systemd/user")
        .join("portzero-daemon.service")
}

#[cfg(target_os = "linux")]
fn install_systemd(binary: &std::path::Path) -> Result<()> {
    let unit_path = systemd_unit_path();
    let unit_dir = unit_path
        .parent()
        .expect("systemd unit path has no parent directory — this is a bug in systemd_unit_path()");
    std::fs::create_dir_all(unit_dir)?;

    let unit = format!(
        r#"[Unit]
Description=Port Zero discovery daemon
Documentation=https://portzero.cloud/docs/daemon

[Service]
Type=simple
ExecStart={binary} start --foreground
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
"#,
        binary = binary.display(),
    );

    std::fs::write(&unit_path, &unit)
        .with_context(|| format!("Failed to write systemd unit to {}", unit_path.display()))?;

    // Reload systemd so it sees the new unit content.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    // Enable the unit (idempotent).
    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "portzero-daemon.service"])
        .status()
        .context("Failed to enable systemd unit")?;
    if !status.success() {
        tracing::warn!("systemctl enable returned non-zero; the unit may already be enabled");
    }

    // Always restart so an already-running (or crash-looping) service picks
    // up the new binary and unit immediately.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "restart", "portzero-daemon.service"])
        .status();

    tracing::info!("Installed systemd user unit: {}", unit_path.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<()> {
    let unit_path = systemd_unit_path();

    if unit_path.exists() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "portzero-daemon.service"])
            .status();

        std::fs::remove_file(&unit_path)
            .with_context(|| format!("Failed to remove systemd unit at {}", unit_path.display()))?;

        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();

        tracing::info!("Uninstalled systemd user unit: {}", unit_path.display());
    }

    Ok(())
}

/// Whether this process runs as LocalSystem (SID S-1-5-18), e.g. as an MSI
/// deferred custom action. Checked via `whoami /user` because the SID is
/// locale-independent, unlike the "NT AUTHORITY\SYSTEM" account name.
#[cfg(target_os = "windows")]
fn running_as_local_system() -> bool {
    std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("\"S-1-5-18\""))
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn install_windows_task(binary: &Path) -> Result<()> {
    use std::process::Command;

    let tr = format!("\"{}\" start --foreground", binary.display());
    let mut args = vec![
        "/Create",
        "/SC",
        "ONLOGON",
        "/TN",
        SERVICE_NAME,
        "/TR",
        &tr,
        "/RL",
        "HIGHEST",
        "/F",
    ];
    // Under LocalSystem (the MSI custom-action context) schtasks cannot map the
    // implicit invoking account into the ONLOGON trigger's UserId — it fails
    // with "No mapping between account names and security IDs was done". An
    // explicit /RU SYSTEM creates the task to run as SYSTEM (which the daemon
    // needs anyway for the TUN device). User-invoked installs keep the default:
    // the task runs as the invoking user at their logon.
    if running_as_local_system() {
        args.extend_from_slice(&["/RU", "SYSTEM"]);
    }

    let output = Command::new("schtasks")
        .args(&args)
        .output()
        .context("Failed to create scheduled task")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to create Windows scheduled task: {}\n\n\
             Try running as administrator, or create a shortcut in your \
             Startup folder manually.",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    tracing::info!("Installed Windows scheduled task: {}", SERVICE_NAME);
    Ok(())
}

#[cfg(target_os = "windows")]
fn uninstall_windows_task() -> Result<()> {
    use std::process::Command;

    let result = Command::new("schtasks")
        .args(["/Delete", "/TN", SERVICE_NAME, "/F"])
        .status()
        .context("Failed to delete scheduled task")?;

    if !result.success() {
        tracing::warn!("schtasks delete returned non-zero; the task may not exist");
    }

    tracing::info!("Uninstalled Windows scheduled task: {}", SERVICE_NAME);
    Ok(())
}

#[cfg(target_os = "windows")]
fn is_windows_task_installed() -> bool {
    use std::process::Command;

    Command::new("schtasks")
        .args(["/Query", "/TN", SERVICE_NAME])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_autostart_installed_default() {
        // On a fresh system, autostart should not be installed
        // (unless the test machine happens to have it)
        let _result = is_autostart_installed();
        // Just ensure it doesn't panic
    }

    #[test]
    fn test_which_nonexistent() {
        let result = which("nonexistent-binary-xyz-123");
        assert!(result.is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_systemd_unit_path() {
        let path = systemd_unit_path();
        assert!(path.to_string_lossy().contains("systemd/user"));
        assert!(path.to_string_lossy().contains("portzero-daemon.service"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_launchd_plist_path() {
        let path = launchd_plist_path();
        // Must be a *system* LaunchDaemon (runs as root), not a user LaunchAgent.
        assert!(path.starts_with("/Library/LaunchDaemons"));
        assert!(!path.to_string_lossy().contains("LaunchAgents"));
        assert!(path.to_string_lossy().contains(SERVICE_NAME));
        assert!(path.to_string_lossy().ends_with(".plist"));
    }

    /// The bootstrap retry loop only kicks in for launchd's asynchronous
    /// "another operation is running" failures. Retrying anything else just
    /// delays a permanent error by the full settle timeout.
    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_busy_errors_are_the_only_retryable_ones() {
        assert!(launchd_is_busy(
            "Bootstrap failed: 37: Operation already in progress"
        ));
        assert!(launchd_is_busy(
            "Boot-out failed: 36: Operation now in progress"
        ));

        assert!(!launchd_is_busy("Bootstrap failed: 5: Input/output error"));
        assert!(!launchd_is_busy(
            "Load failed: 1: Operation not permitted while System Integrity Protection is engaged"
        ));
    }

    /// A launchctl failure that printed nothing must still say something — the
    /// point of capturing its output is that the user sees a reason.
    #[cfg(target_os = "macos")]
    #[test]
    fn silent_launchctl_failures_still_describe_themselves() {
        let out = std::process::Command::new("/usr/bin/false")
            .output()
            .expect("/usr/bin/false should run");
        let described = describe_launchctl(&out);
        assert!(
            described.contains("printed nothing"),
            "described was: {described}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_render_launchd_plist_is_well_formed_root_daemon() {
        let plist = render_launchd_plist("/usr/local/bin/portzero", DAEMON_LOG_DIR, None);

        // Valid plist scaffolding.
        assert!(plist.contains("<!DOCTYPE plist"));
        assert!(plist.contains("<plist version=\"1.0\">"));
        assert!(plist.trim_end().ends_with("</plist>"));

        // Correct label + ProgramArguments [binary, "start", "--foreground"].
        assert!(plist.contains(&format!("<string>{SERVICE_NAME}</string>")));
        assert!(plist.contains("<string>/usr/local/bin/portzero</string>"));
        assert!(plist.contains("<string>start</string>"));
        assert!(plist.contains("<string>--foreground</string>"));

        // Daemon keys.
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>ProcessType</key>"));
        assert!(plist.contains("<string>Background</string>"));

        // Logs go to a root-writable path, NOT the user's home.
        assert!(plist.contains("/Library/Logs/portzero/daemon.log"));
        assert!(!plist.contains("/.devenv"));

        // No EnvironmentVariables block when home_dir is None.
        assert!(!plist.contains("EnvironmentVariables"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_render_launchd_plist_embeds_home_dir() {
        let plist = render_launchd_plist(
            "/usr/local/bin/portzero",
            DAEMON_LOG_DIR,
            Some("/Users/alice"),
        );

        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(plist.contains("<key>HOME</key>"));
        assert!(plist.contains("<string>/Users/alice</string>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_macos_passwd_home_finds_user() {
        // Build a minimal /etc/passwd-style string and verify parsing.
        // We test the logic inline since macos_passwd_home reads the real file.
        let content = "root:*:0:0:System Administrator:/var/root:/bin/sh\nalice:*:501:20:Alice:/Users/alice:/bin/zsh\n";
        let home: Option<String> = content.lines().find_map(|line| {
            let mut fields = line.splitn(7, ':');
            let name = fields.next()?;
            if name != "alice" {
                return None;
            }
            let home = fields.nth(4)?;
            Some(home.to_string())
        });
        assert_eq!(home, Some("/Users/alice".to_string()));
    }
}
