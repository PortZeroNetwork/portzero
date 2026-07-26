//! The overlay-active check and the reasoning behind its message.
//!
//! Split out of `doctor.rs` because explaining *why* the overlay is down is the
//! most nuanced judgement the doctor makes — it has to distinguish a genuine
//! privileges failure from a daemon that is merely still starting, and it must
//! not assert either without checking.

use portzero_daemon::route_table::OverlayState;

use super::Check;

/// Is the overlay network actually active? This is the launch-blocking case:
/// the daemon can report "running" while the overlay never came up (started
/// unprivileged, or startup wedged), so `.portzero.local` names never resolve.
///
/// `overlay_active` in `overlay.json` is written `true` only after the daemon
/// successfully creates the TUN device (which needs root / CAP_NET_ADMIN), so it
/// is the authoritative signal for this failure mode.
pub(crate) fn check_overlay_active(pid: Option<u32>, overlay: &OverlayState) -> Check {
    if pid.is_none() {
        return Check::fail(
            "overlay active",
            "overlay inactive — the daemon is not running",
            "portzero start",
        );
    }

    if overlay.overlay_active {
        return Check::pass(
            "overlay active",
            "TUN device up, virtual IPs served in 10.254.0.0/16 (gateway 10.254.0.1)",
        );
    }

    let (detail, fix) = inactive_overlay_report(pid.and_then(daemon_is_privileged));
    Check::fail("overlay active", detail, fix)
}

/// Describe an inactive overlay without asserting a cause that was never checked.
///
/// This used to report "daemon was started without root/CAP_NET_ADMIN"
/// unconditionally. That is only *one* reason the overlay can be down, and
/// stating it as fact actively misleads: on a machine whose root LaunchDaemon
/// was simply still starting — the overlay comes up several seconds after the
/// daemon writes its PID file — doctor confidently blamed privileges, sending
/// the user to `sudo portzero autostart enable` for a daemon that was already
/// running as root. Whoever reads this output is usually debugging precisely
/// because they don't know the cause, so a guess presented as a finding is worse
/// than an honest "here is what I could and could not determine".
///
/// `privileged` is whether the daemon process runs as root, or `None` when that
/// could not be determined.
fn inactive_overlay_report(privileged: Option<bool>) -> (String, String) {
    match privileged {
        Some(false) => (
            "overlay inactive — the daemon is not running as root, so it could not create \
             the TUN device that serves .portzero.local"
                .to_string(),
            overlay_fix_hint(),
        ),
        Some(true) => (
            "overlay inactive — but the daemon IS running with the required privileges, so \
             this is not a permissions problem. Either it is still starting (the overlay \
             comes up a few seconds after the daemon does) or creating the TUN device failed"
                .to_string(),
            format!(
                "wait a few seconds and re-run `portzero doctor`; if it persists, look for \
                 the overlay startup steps in {}",
                daemon_log_hint()
            ),
        ),
        None => (
            "overlay inactive — .portzero.local names will not resolve. Could not determine \
             whether the daemon has the privileges the overlay needs"
                .to_string(),
            overlay_fix_hint(),
        ),
    }
}

/// Whether the process is running as root. `None` when it cannot be determined
/// (the process is gone, `ps` is unavailable, or the platform has no such
/// notion), so callers can say "unknown" rather than guess.
#[cfg(unix)]
fn daemon_is_privileged(pid: u32) -> Option<bool> {
    let out = std::process::Command::new("ps")
        .args(["-o", "uid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let uid: u32 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    Some(uid == 0)
}

#[cfg(not(unix))]
fn daemon_is_privileged(_pid: u32) -> Option<bool> {
    // Windows elevation is not a uid comparison; report it as undetermined
    // rather than inventing an answer.
    None
}

/// Where to look for the daemon's own account of what happened.
fn daemon_log_hint() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "/Library/Logs/portzero/daemon.log (root service) or ~/.portzero/daemon/daemon.log"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "~/.portzero/daemon/daemon.log"
    }
}

/// Per-platform command to bring the overlay up with the required privileges.
fn overlay_fix_hint() -> String {
    #[cfg(target_os = "macos")]
    {
        "sudo portzero autostart enable".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        "sudo setcap 'cap_net_admin,cap_net_bind_service+eip' $(which portzero), then restart the daemon".to_string()
    }
    #[cfg(target_os = "windows")]
    {
        "run the daemon as Administrator and ensure wintun.dll sits next to portzero.exe"
            .to_string()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "start the daemon with privileges to create a TUN device".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A privileged daemon whose overlay is down must not be blamed for a
    /// permissions problem it does not have. Doing so cost a real debugging
    /// session: doctor said "started without root" about a root LaunchDaemon
    /// that was merely three seconds into startup.
    #[test]
    fn a_privileged_daemon_is_never_blamed_for_privileges() {
        let (detail, fix) = inactive_overlay_report(Some(true));
        assert!(
            !detail.contains("not running as root"),
            "detail was: {detail}"
        );
        assert!(
            detail.contains("still starting"),
            "the real alternative cause must be offered: {detail}"
        );
        assert!(
            !fix.contains("autostart enable"),
            "must not send a root daemon to a privileges fix: {fix}"
        );
    }

    #[test]
    fn an_unprivileged_daemon_still_gets_the_privileges_fix() {
        let (detail, fix) = inactive_overlay_report(Some(false));
        assert!(detail.contains("not running as root"), "detail: {detail}");
        assert_eq!(fix, overlay_fix_hint());
    }

    /// When privilege can't be determined, say so rather than assuming either way.
    #[test]
    fn undetermined_privilege_is_reported_as_undetermined() {
        let (detail, _) = inactive_overlay_report(None);
        assert!(detail.contains("Could not determine"), "detail: {detail}");
    }
}
