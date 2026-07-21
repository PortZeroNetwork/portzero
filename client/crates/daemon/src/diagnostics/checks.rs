//! Individual diagnostic checks, grouped by the Binary, Network, DNS, Auth, and
//! System categories. Each returns `Some(Diagnostic)` when it finds a problem
//! and `None` when the check passes.
//!
//! The TLS / local-CA trust checks live in `checks_tls_trust.rs`.

use std::path::Path;
// Still needed on Linux by check_binary_exists' " (deleted)" strip below, even
// though the TLS-trust split took the other users out of this file.
#[cfg(target_os = "linux")]
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

use super::{Diagnostic, Fix, FixKind, Severity, PORTZERO_LOCAL_DASHBOARD_IP};

pub(super) fn check_binary_exists() -> Option<Diagnostic> {
    let exe = std::env::current_exe().ok()?;

    // On Linux, `current_exe()` resolves the `/proc/self/exe` symlink. If the
    // running process's binary was replaced in place (e.g. a self-update
    // renaming the new binary over the old path) the kernel appends
    // " (deleted)" to the target because the *running* process still holds
    // the old, now-unlinked inode — even though a good file exists at the
    // real path. Strip that marker and re-check the real path before
    // reporting a false "binary missing" critical diagnostic.
    #[cfg(target_os = "linux")]
    let exe: PathBuf = exe
        .to_str()
        .and_then(|s| s.strip_suffix(" (deleted)"))
        .map(PathBuf::from)
        .unwrap_or(exe);

    if std::fs::metadata(&exe).is_err() {
        tracing::debug!("check_binary_exists: binary not found at {}", exe.display());
        Some(Diagnostic {
            id: "binary_missing".into(),
            severity: Severity::Critical,
            category: "binary".into(),
            title: "portzero binary not found at its own path".to_string(),
            detail: format!("The binary was expected at {} but could not be accessed.", exe.display()),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Antivirus may have quarantined or deleted the file. Add the portzero installation directory to your antivirus exclusions.".to_string(),
                command: None,
            }),
        })
    } else {
        None
    }
}

/// Whether a process is a genuine `portzero` daemon instance (as opposed to a
/// one-shot CLI invocation like `portzero status`, or a same-named companion
/// binary like `portzero-tray` / `portzero-app`).
///
/// The CLI and daemon are the same executable (`portzero`), distinguished only
/// by the hidden `start --foreground` arguments `spawn_daemon` re-execs with
/// (see `cli/src/daemon.rs`). Matching on the binary name alone previously
/// counted `portzero-tray` and `portzero-app` — which legitimately run
/// alongside the daemon on every normal install — as extra "instances", and
/// even an exact-name match would still count a passing `portzero status`
/// invocation. Requiring `--foreground` in the arguments avoids both.
fn is_daemon_process(p: &sysinfo::Process) -> bool {
    let name = p.name().to_string_lossy();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    if stem != "portzero" {
        return false;
    }
    p.cmd()
        .iter()
        .any(|arg| arg.to_string_lossy() == "--foreground")
}

pub(super) fn check_multiple_instances() -> Option<Diagnostic> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    // (pid, start_time) for every genuine daemon process, oldest first.
    let mut daemons: Vec<(u32, u64)> = sys
        .processes()
        .iter()
        .filter(|(_, p)| is_daemon_process(p))
        .map(|(pid, p)| (pid.as_u32(), p.start_time()))
        .collect();
    daemons.sort_by_key(|&(_, start_time)| start_time);

    if daemons.len() > 1 {
        tracing::debug!("check_multiple_instances: {} daemons found", daemons.len());
        let newest_pid = daemons.last().expect("len > 1").0;
        let older_pids: Vec<u32> = daemons[..daemons.len() - 1]
            .iter()
            .map(|&(p, _)| p)
            .collect();
        let older_pids_display = older_pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let older_pids_cmd = older_pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        Some(Diagnostic {
            id: "multiple_instances".into(),
            severity: Severity::Warning,
            category: "binary".into(),
            title: "Multiple portzero daemons detected".to_string(),
            detail: format!(
                "{} `portzero start --foreground` daemon processes are running (pids {}, {}). \
                 This can cause route conflicts.",
                daemons.len(),
                older_pids_display,
                newest_pid
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: format!(
                    "Stop the older daemon instance(s) (pid {older_pids_display}), keeping the \
                     newest (pid {newest_pid}). This targets only the identified daemon \
                     processes — it will not touch portzero-tray, portzero-app, or other \
                     `portzero` CLI commands."
                ),
                command: Some(format!("kill {older_pids_cmd}")),
            }),
        })
    } else {
        None
    }
}

pub(super) fn check_state_dir_writable(state_dir: &Path) -> Option<Diagnostic> {
    let test_path = state_dir.join(".write_test");
    match std::fs::write(&test_path, b"ok") {
        Ok(_) => {
            let _ = std::fs::remove_file(&test_path);
            None
        }
        Err(e) => {
            tracing::debug!("check_state_dir_writable: {}: {}", state_dir.display(), e);
            Some(Diagnostic {
                id: "state_dir_unwritable".into(),
                severity: Severity::Error,
                category: "system".into(),
                title: "State directory is not writable".to_string(),
                detail: format!("Cannot write to {}: {}", state_dir.display(), e),
                fix: Some(Fix {
                    kind: FixKind::Manual,
                    description: "Check permissions on the state directory".to_string(),
                    command: None,
                }),
            })
        }
    }
}

pub(super) fn check_state_files_valid(state_dir: &Path) -> Option<Diagnostic> {
    let files = ["routes.json", "overlay.json", "cloud_state.json"];
    let mut failed: Vec<&str> = Vec::new();

    for name in &files {
        let path = state_dir.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if serde_json::from_str::<serde_json::Value>(&content).is_err() {
                tracing::debug!("check_state_files_valid: {} failed to parse", name);
                failed.push(name);
            }
        }
        // Missing files are fine — skip them
    }

    if failed.is_empty() {
        None
    } else {
        Some(Diagnostic {
            id: "state_file_corrupt".into(),
            severity: Severity::Warning,
            category: "system".into(),
            title: "State file corrupted".to_string(),
            detail: format!(
                "The following state files failed to parse: {}",
                failed.join(", ")
            ),
            fix: Some(Fix {
                kind: FixKind::Auto,
                description: "Restart the daemon to reinitialize state files".to_string(),
                command: Some("portzero restart".to_string()),
            }),
        })
    }
}

#[cfg(unix)]
pub(super) fn check_fd_limit() -> Option<Diagnostic> {
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let ret = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
    if ret != 0 {
        tracing::warn!("check_fd_limit: getrlimit failed");
        return None;
    }
    let soft = rlim.rlim_cur;
    if soft < 1024 {
        tracing::debug!("check_fd_limit: soft limit is {}", soft);
        Some(Diagnostic {
            id: "fd_limit_low".into(),
            severity: Severity::Warning,
            category: "system".into(),
            title: "File descriptor limit is low".to_string(),
            detail: format!("Current soft limit: {}. Recommended: ≥1024.", soft),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Increase the file descriptor limit.".to_string(),
                command: Some(
                    "ulimit -n 65536  # add to ~/.bashrc or /etc/security/limits.conf".to_string(),
                ),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(unix))]
pub(super) fn check_fd_limit() -> Option<Diagnostic> {
    None
}

pub(super) fn check_etc_hosts_override() -> Option<Diagnostic> {
    #[cfg(unix)]
    let hosts_path = "/etc/hosts";
    #[cfg(windows)]
    let hosts_path = r"C:\Windows\System32\drivers\etc\hosts";

    let content = match std::fs::read_to_string(hosts_path) {
        Ok(c) => c,
        Err(_) => return None,
    };

    let analysis = analyze_portzero_hosts_entries(&content);

    if analysis.conflicting_lines.is_empty() {
        None
    } else {
        tracing::debug!(
            "check_etc_hosts_override: found {} conflicting lines",
            analysis.conflicting_lines.len()
        );
        Some(Diagnostic {
            id: "etc_hosts_override".into(),
            severity: Severity::Warning,
            category: "dns".into(),
            title: "/etc/hosts contains custom portzero.local overrides".to_string(),
            detail: format!(
                "The following lines in {} override portzero.local with unexpected values:\n{}",
                hosts_path,
                analysis.conflicting_lines.join("\n")
            ),
            fix: Some(Fix {
                kind: FixKind::Confirm,
                description: format!(
                    "Keep only the expected dashboard pin ({PORTZERO_LOCAL_DASHBOARD_IP} portzero.local) or remove custom overrides"
                ),
                command: Some(r"sudo sed -i '/portzero\.local/d' /etc/hosts".to_string()),
            }),
        })
    }
}

#[cfg(target_os = "linux")]
pub(super) fn check_dev_net_tun() -> Option<Diagnostic> {
    if std::fs::metadata("/dev/net/tun").is_err() {
        tracing::debug!("check_dev_net_tun: /dev/net/tun not available");
        Some(Diagnostic {
            id: "dev_net_tun_missing".into(),
            severity: Severity::Critical,
            category: "network".into(),
            title: "/dev/net/tun is not available".to_string(),
            detail: "The TUN kernel module is required to create virtual network devices."
                .to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Load the tun kernel module.".to_string(),
                command: Some("sudo modprobe tun".to_string()),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn check_dev_net_tun() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
pub(super) fn check_cap_net_admin() -> Option<Diagnostic> {
    let content = match std::fs::read_to_string("/proc/self/status") {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("check_cap_net_admin: failed to read /proc/self/status: {e}");
            return None;
        }
    };

    let cap_eff_hex = content
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:").map(|v| v.trim().to_string()));

    let cap_eff = match cap_eff_hex
        .as_deref()
        .and_then(|h| u64::from_str_radix(h, 16).ok())
    {
        Some(v) => v,
        None => {
            tracing::warn!("check_cap_net_admin: could not parse CapEff");
            return None;
        }
    };

    const CAP_NET_ADMIN: u64 = 1 << 12;
    if cap_eff & CAP_NET_ADMIN == 0 {
        tracing::debug!(
            "check_cap_net_admin: CAP_NET_ADMIN not set (CapEff={:#x})",
            cap_eff
        );
        let exe_path = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/path/to/portzero".to_string());
        Some(Diagnostic {
            id: "cap_net_admin_missing".into(),
            severity: Severity::Critical,
            category: "network".into(),
            title: "CAP_NET_ADMIN capability is not set".to_string(),
            detail: "The daemon cannot create TUN devices without CAP_NET_ADMIN.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Grant the Linux capabilities required for the overlay network."
                    .to_string(),
                command: Some(format!(
                    "sudo setcap 'cap_net_admin,cap_net_bind_service+eip' {}",
                    exe_path
                )),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn check_cap_net_admin() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
pub(super) fn check_ptrace_scope() -> Option<Diagnostic> {
    let content = match std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope") {
        Ok(c) => c,
        Err(_) => return None, // file absent on non-Yama kernels
    };

    let value: u32 = match content.trim().parse() {
        Ok(v) => v,
        Err(_) => return None,
    };

    if value >= 2 {
        tracing::debug!("check_ptrace_scope: ptrace_scope={}", value);
        Some(Diagnostic {
            id: "ptrace_scope_restricted".into(),
            severity: Severity::Warning,
            category: "system".into(),
            title: "ptrace_scope restricts process environment reading".to_string(),
            detail: format!(
                "ptrace_scope={}. The daemon cannot read PZ_TUNNEL from other processes. Values ≥2 require processes to opt in.",
                value
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Relax the ptrace scope to allow environment variable reading.".to_string(),
                command: Some(
                    "echo 1 | sudo tee /proc/sys/kernel/yama/ptrace_scope".to_string(),
                ),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn check_ptrace_scope() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
pub(super) fn check_avahi_conflict() -> Option<Diagnostic> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let found = sys
        .processes()
        .values()
        .any(|p| p.name().to_string_lossy() == "avahi-daemon");

    if found && avahi_can_intercept_portzero_local() {
        tracing::debug!("check_avahi_conflict: avahi-daemon is running");
        Some(Diagnostic {
            id: "avahi_conflict".into(),
            severity: Severity::Warning,
            category: "dns".into(),
            title: "avahi-daemon may intercept bare portzero.local lookups".to_string(),
            detail: "This system appears to prefer mDNS for single-label .local lookups and does not have the expected portzero.local hosts pin. The dashboard name may resolve unreliably until that is fixed.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Add the expected portzero.local hosts pin or re-run the Linux installer; stopping avahi-daemon is optional".to_string(),
                command: Some(format!(
                    "echo '{PORTZERO_LOCAL_DASHBOARD_IP} portzero.local # portzero-local' | sudo tee -a /etc/hosts"
                )),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn check_avahi_conflict() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
pub(super) fn check_systemd_resolved() -> Option<Diagnostic> {
    let dir_exists = std::fs::metadata("/run/systemd/resolve/").is_ok();
    let resolv_exists = std::fs::metadata("/run/systemd/resolve/resolv.conf").is_ok();

    if !dir_exists && !resolv_exists {
        tracing::debug!("check_systemd_resolved: systemd-resolved not running");
        Some(Diagnostic {
            id: "systemd_resolved_not_running".into(),
            severity: Severity::Error,
            category: "dns".into(),
            title: "systemd-resolved is not running".to_string(),
            detail: "portzero uses systemd-resolved to register the scoped .portzero.local resolver. Without it, .portzero.local names will not resolve.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Enable and start systemd-resolved.".to_string(),
                command: Some("sudo systemctl enable --now systemd-resolved".to_string()),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn check_systemd_resolved() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "macos")]
pub(super) fn check_macos_resolver_file() -> Option<Diagnostic> {
    if std::fs::metadata("/etc/resolver/portzero.local").is_err() {
        tracing::debug!("check_macos_resolver_file: /etc/resolver/portzero.local missing");
        Some(Diagnostic {
            id: "macos_resolver_missing".into(),
            severity: Severity::Error,
            category: "dns".into(),
            title: "/etc/resolver/portzero.local is missing".to_string(),
            detail: "macOS uses this file to route .portzero.local DNS queries to the portzero resolver.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Confirm,
                description: "Restart the portzero daemon with admin privileges to recreate the resolver file".to_string(),
                command: Some("sudo portzero restart".to_string()),
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn check_macos_resolver_file() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "windows")]
pub(super) fn check_wintun_present() -> Option<Diagnostic> {
    let missing = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wintun.dll")))
        .map(|dll| !dll.exists())
        .unwrap_or(false);

    if missing {
        tracing::debug!("check_wintun_present: wintun.dll not found");
        Some(Diagnostic {
            id: "wintun_missing".into(),
            severity: Severity::Critical,
            category: "network".into(),
            title: "wintun.dll is missing".to_string(),
            detail: "portzero requires Wintun to create the virtual network adapter on Windows.".to_string(),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "Re-run the portzero installer, or download wintun.dll from https://wintun.net and place it next to portzero.exe".to_string(),
                command: None,
            }),
        })
    } else {
        None
    }
}

#[cfg(not(target_os = "windows"))]
pub(super) fn check_wintun_present() -> Option<Diagnostic> {
    None
}

/// Detect out-of-band removal of the `10.254.0.2 portzero.local` dashboard pin
/// that `portzero setup` (macOS) / the Linux installer add to `/etc/hosts`.
///
/// Only macOS and Linux use the hosts pin: macOS needs it because
/// mDNSResponder answers bare `.local` names before the scoped resolver, and the
/// Linux installer adds it because nss-mdns claims two-label `.local` names. On
/// Windows the NRPT rule covers `portzero.local` itself, so there is no pin to
/// regress. A *wrong* override is already reported by
/// [`check_etc_hosts_override`]; this only fires when the pin is genuinely
/// absent, so the two checks never double-report.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn check_dashboard_hosts_pin() -> Option<Diagnostic> {
    let content = match std::fs::read_to_string("/etc/hosts") {
        Ok(c) => c,
        Err(_) => return None,
    };

    let analysis = analyze_portzero_hosts_entries(&content);
    if analysis.has_expected_dashboard_pin || !analysis.conflicting_lines.is_empty() {
        return None;
    }

    tracing::debug!("check_dashboard_hosts_pin: expected dashboard pin is missing");
    let safety = crate::hosts::check_hosts_write_safety(Path::new("/etc/hosts"));
    let mut detail = format!(
        "/etc/hosts no longer contains the expected `{PORTZERO_LOCAL_DASHBOARD_IP} portzero.local` \
         entry, so the dashboard name may not resolve."
    );
    if !safety.is_safe() {
        detail.push_str("\n`sudo portzero setup` is unlikely to restore it on its own:");
        for blocker in &safety.blockers {
            let (label, explanation) = blocker.describe();
            detail.push_str(&format!("\n  - {label}: {explanation}"));
        }
    }
    Some(Diagnostic {
        id: "dashboard_hosts_pin_missing".into(),
        severity: Severity::Warning,
        category: "dns".into(),
        title: "The portzero.local dashboard hosts pin is missing".to_string(),
        detail,
        fix: Some(Fix {
            kind: FixKind::Confirm,
            description: "Re-run setup to restore the dashboard hosts pin.".to_string(),
            command: Some("sudo portzero setup".to_string()),
        }),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn check_dashboard_hosts_pin() -> Option<Diagnostic> {
    None
}

/// Detect out-of-band removal of the autostart service that `portzero setup` /
/// the installers register (macOS root LaunchDaemon, Linux systemd user unit,
/// Windows scheduled task).
///
/// The daemon that runs this check is, by definition, currently running — so
/// this is not about "is the daemon up" but about whether it will come back
/// after a reboot. If the service definition was removed out-of-band the daemon
/// keeps running now but silently stops surviving restarts.
pub(super) fn check_autostart_installed() -> Option<Diagnostic> {
    if crate::autostart::is_autostart_installed() {
        return None;
    }

    // macOS installs the service as a root LaunchDaemon via `portzero setup`
    // (sudo); Linux/Windows can (re)install it unprivileged with `autostart
    // enable`.
    #[cfg(target_os = "macos")]
    let command = "sudo portzero setup";
    #[cfg(not(target_os = "macos"))]
    let command = "portzero autostart enable";

    tracing::debug!("check_autostart_installed: autostart service not installed");
    Some(Diagnostic {
        id: "autostart_missing".into(),
        severity: Severity::Warning,
        category: "system".into(),
        title: "Autostart service is not installed".to_string(),
        detail: "The portzero daemon is running now but is not registered to start automatically, \
                 so it will not come back after a reboot."
            .to_string(),
        fix: Some(Fix {
            kind: FixKind::Confirm,
            description: "Reinstall the autostart service so the daemon starts at boot."
                .to_string(),
            command: Some(command.to_string()),
        }),
    })
}

pub(super) fn check_conflicting_vpn_software() -> Option<Diagnostic> {
    const VPN_NAMES: &[&str] = &[
        "tailscaled",
        "tailscale",
        "openconnect",
        "vpnc",
        "openvpn",
        "GlobalProtect",
        "PanGPS",
        "ciscovpn",
        "vpnagentd",
        "ZscalerApp",
        "wg-quick",
        "Pulse Secure",
        "PulseSecureService",
        "ivpnagent",
    ];

    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let detected: Vec<String> = sys
        .processes()
        .values()
        .filter_map(|p| {
            let name = p.name().to_string_lossy();
            if VPN_NAMES.iter().any(|vpn| name.contains(vpn)) {
                Some(name.into_owned())
            } else {
                None
            }
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if detected.is_empty() {
        None
    } else {
        tracing::debug!("check_conflicting_vpn_software: detected {:?}", detected);
        let mut sorted = detected;
        sorted.sort();
        Some(Diagnostic {
            id: "vpn_software_detected".into(),
            severity: Severity::Info,
            category: "network".into(),
            title: "VPN software is running".to_string(),
            detail: format!(
                "Detected: {}. VPNs may conflict with the 10.254.0.0/16 overlay range or intercept DNS.",
                sorted.join(", ")
            ),
            fix: Some(Fix {
                kind: FixKind::Manual,
                description: "If .portzero.local names don't resolve, check your VPN's split-tunneling settings to exclude 10.254.0.0/16".to_string(),
                command: None,
            }),
        })
    }
}

pub(super) fn check_auth_token(state_dir: &Path) -> Option<Diagnostic> {
    // config_dir is ~/.portzero/ (parent of the daemon's state_dir ~/.portzero/daemon/)
    let config_dir = state_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| state_dir.to_path_buf());

    let auth = crate::auth::AuthConfig::load_from(&config_dir);

    match auth.token {
        None => {
            tracing::debug!("check_auth_token: no token present");
            Some(Diagnostic {
                id: "auth_not_logged_in".into(),
                severity: Severity::Info,
                category: "auth".into(),
                title: "Not logged in to portzero.cloud".to_string(),
                detail: "Cloud tunnels are unavailable. Run `portzero login` to enable them."
                    .to_string(),
                fix: Some(Fix {
                    kind: FixKind::Manual,
                    description: "Log in to enable cloud tunnel support.".to_string(),
                    command: Some("portzero login".to_string()),
                }),
            })
        }
        Some(token) => {
            // Decode JWT payload (index 1) to get `exp`
            let exp: Option<u64> = token.split('.').nth(1).and_then(|b64| {
                let bytes = URL_SAFE_NO_PAD.decode(b64).ok()?;
                let val: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                val.get("exp")?.as_u64()
            });

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            match exp {
                None => None, // can't decode expiry — assume valid
                Some(exp) if exp < now => {
                    tracing::debug!("check_auth_token: token expired at {}", exp);
                    Some(Diagnostic {
                        id: "auth_token_expired".into(),
                        severity: Severity::Warning,
                        category: "auth".into(),
                        title: "Cloud auth token has expired".to_string(),
                        detail: "The stored auth token is no longer valid. Re-authenticate to restore cloud tunnel access.".to_string(),
                        fix: Some(Fix {
                            kind: FixKind::Manual,
                            description: "Re-authenticate with portzero.cloud.".to_string(),
                            command: Some("portzero login".to_string()),
                        }),
                    })
                }
                Some(exp) if exp < now + 86400 => {
                    tracing::debug!("check_auth_token: token expires soon at {}", exp);
                    Some(Diagnostic {
                        id: "auth_token_expires_soon".into(),
                        severity: Severity::Info,
                        category: "auth".into(),
                        title: "Cloud auth token expires soon (within 24 hours)".to_string(),
                        detail: "The daemon should auto-refresh this token before it expires. Re-authenticate only if cloud reconnects start failing."
                            .to_string(),
                        fix: Some(Fix {
                            kind: FixKind::Manual,
                            description: "Optional: refresh your login now if you want to rotate it manually.".to_string(),
                            command: Some("portzero login".to_string()),
                        }),
                    })
                }
                Some(_) => None, // token is valid and not expiring soon
            }
        }
    }
}

/// Check whether the user is trying to use cloud tunnels without permission — either on a
/// free (or unknown) plan, and not a member of any team whose plan grants cloud tunnels.
/// This produces a visible upsell prompt in `portzero status` and the web dashboard.
pub(super) fn check_cloud_plan(state_dir: &Path) -> Option<Diagnostic> {
    // Only relevant if logged in
    let config_dir = state_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| state_dir.to_path_buf());
    let auth = crate::auth::AuthConfig::load_from(&config_dir);
    auth.token?;

    let can_use_cloud_tunnels =
        crate::discovery_loop::read_cloud_can_use_tunnels_from_path(state_dir);
    let lacks_permission = match can_use_cloud_tunnels {
        Some(true) => false,
        Some(false) => true,
        None => true, // haven't seen Welcome yet but trying cloud?
    };
    if !lacks_permission {
        return None;
    }

    // Do we have (or are attempting) any cloud routes?
    let routes_path = state_dir.join("routes.json");
    let has_cloud_route = std::fs::read_to_string(&routes_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("routes").cloned())
        .and_then(|r| {
            if let Some(obj) = r.as_object() {
                Some(obj.keys().any(|k| k.contains(".portzero.cloud")))
            } else {
                r.as_array().map(|arr| {
                    arr.iter().any(|row| {
                        row.get("domain")
                            .and_then(|d| d.as_str())
                            .is_some_and(|d| d.contains(".portzero.cloud"))
                    })
                })
            }
        })
        .unwrap_or(false);

    if !has_cloud_route {
        return None;
    }

    Some(Diagnostic {
        id: "cloud_plan_required".into(),
        severity: Severity::Warning,
        category: "auth".into(),
        title: "Cloud tunnels require a paid plan".to_string(),
        detail:
            "You are using *.tunnel.portzero.cloud domains but your current plan does not include them. \
                 Local .portzero.local tunnels continue to work for free."
                .to_string(),
        fix: Some(Fix {
            kind: FixKind::Manual,
            description: "Upgrade to use portzero.cloud tunnels.".to_string(),
            command: Some("open https://app.portzero.cloud  # or visit in browser".to_string()),
        }),
    })
}

#[derive(Debug, Default)]
pub(super) struct PortzeroHostsAnalysis {
    pub(super) conflicting_lines: Vec<String>,
    pub(super) has_expected_dashboard_pin: bool,
}

pub(super) fn analyze_portzero_hosts_entries(content: &str) -> PortzeroHostsAnalysis {
    let mut analysis = PortzeroHostsAnalysis::default();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let before_comment = trimmed.split('#').next().unwrap_or(trimmed);
        let mut parts = before_comment.split_whitespace();
        let Some(ip) = parts.next() else {
            continue;
        };

        let names: Vec<&str> = parts.collect();
        if !names.contains(&"portzero.local") {
            continue;
        }

        if ip == PORTZERO_LOCAL_DASHBOARD_IP {
            analysis.has_expected_dashboard_pin = true;
        } else {
            analysis.conflicting_lines.push(line.to_string());
        }
    }

    analysis
}

#[cfg(target_os = "linux")]
fn avahi_can_intercept_portzero_local() -> bool {
    let hosts_analysis = std::fs::read_to_string("/etc/hosts")
        .ok()
        .map(|content| analyze_portzero_hosts_entries(&content))
        .unwrap_or_default();
    if hosts_analysis.has_expected_dashboard_pin {
        return false;
    }

    std::fs::read_to_string("/etc/nsswitch.conf")
        .ok()
        .as_deref()
        .is_some_and(nsswitch_prefers_mdns_for_local_hosts)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn nsswitch_prefers_mdns_for_local_hosts(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("hosts:")
            && trimmed.contains("mdns")
            && trimmed.contains("[NOTFOUND=return]")
    })
}
