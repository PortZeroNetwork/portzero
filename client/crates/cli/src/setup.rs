//! One-shot privileged setup for package managers and first-run instructions.

use std::io::Write;

use anyhow::{Context, Result};

use crate::{autostart, trust};

const HOSTS_PATH: &str = "/etc/hosts";
const DASHBOARD_HOSTS_LINE: &str = "10.254.0.2 portzero.local # portzero-local";
const DASHBOARD_HOST: &str = "portzero.local";
const DASHBOARD_IP: &str = "10.254.0.2";

/// Run the privileged setup steps that package managers should not execute
/// automatically: trust install, autostart install/start, and the dashboard
/// hosts pin needed for macOS `.local` behavior.
///
/// The steps are independent, so setup is **best-effort**: a failure in one step
/// (e.g. the OS trust store declining to add the CA without an interactive
/// authorization prompt) must not skip the others and leave the machine
/// half-configured with no daemon, resolver, or hosts pin. Every step runs; any
/// failures are collected, reported at the end with their actionable messages,
/// and surfaced as a non-zero exit so callers/packagers still see the problem.
pub async fn run() -> Result<()> {
    println!("PortZero setup will make these system changes:");
    println!("  - generate the local CA if it does not already exist");
    println!("  - install the local CA into available OS/browser trust stores");
    println!("  - install and start the PortZero autostart daemon");
    println!("  - ensure the scoped .portzero.local DNS resolver is installed");
    println!("  - ensure /etc/hosts contains: {DASHBOARD_HOSTS_LINE}");
    println!();

    let mut failures: Vec<(&str, anyhow::Error)> = Vec::new();

    println!("Generating local CA...");
    if let Err(err) = trust::generate() {
        failures.push(("generate the local CA", err));
    }

    println!("Installing local CA trust...");
    if let Err(err) = trust::install() {
        failures.push(("install the local CA into OS/browser trust stores", err));
    }

    println!("Installing and starting autostart daemon...");
    if let Err(err) = autostart::enable() {
        failures.push(("install and start the autostart daemon", err));
    }

    println!("Ensuring scoped .portzero.local DNS resolver...");
    if let Err(err) = ensure_scoped_resolver().await {
        failures.push(("install the scoped .portzero.local resolver", err));
    }

    println!("Ensuring dashboard hosts entry...");
    if let Err(err) = ensure_dashboard_hosts_entry(HOSTS_PATH) {
        failures.push(("pin the dashboard /etc/hosts entry", err));
    }

    #[cfg(target_os = "macos")]
    {
        println!("Installing the system-tray companion...");
        if let Err(err) = install_tray_agent() {
            failures.push(("install the system-tray login agent", err));
        }
    }

    println!("Restoring ownership of your PortZero directories...");
    if let Err(err) = restore_user_ownership() {
        failures.push(("restore ownership of your PortZero directories", err));
    }

    if failures.is_empty() {
        println!();
        println!("Setup complete.");
        println!("Run an example from the Getting Started section in the PortZero app.");
        // Launch the desktop app, but only once the daemon actually answers —
        // opening early would show a daemon-down state before DNS/overlay are ready.
        wait_and_open_app().await;
        return Ok(());
    }

    eprintln!();
    eprintln!("Setup finished with {} problem(s):", failures.len());
    for (step, err) in &failures {
        eprintln!("  - could not {step}: {err:#}");
    }
    anyhow::bail!(
        "setup finished with {} failed step(s); re-run with administrator privileges \
         or address the problems listed above",
        failures.len()
    )
}

/// Wait until the daemon answers, then launch the PortZero desktop app.
/// Best-effort: prints a hint and returns rather than failing setup if the
/// daemon never becomes reachable (or the app can't be launched). The readiness
/// probe bypasses any proxy so a corporate `HTTP_PROXY` can't swallow the local
/// request.
async fn wait_and_open_app() {
    // Probe the overlay's fixed dashboard IP with an explicit Host header
    // instead of resolving `portzero.local`. macOS mDNSResponder claims every
    // `.local` name before the scoped resolver is consulted, so
    // getaddrinfo(portzero.local) can stall for tens of seconds on a machine
    // whose overlay is already serving happily. Probing the IP measures the
    // daemon rather than the resolver, which is what this wait is actually for.
    let probe_url = format!("http://{DASHBOARD_IP}/status.json");
    const OVERALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    print!("Waiting for the daemon to become ready");
    let _ = std::io::stdout().flush();

    // A wall-clock deadline, not an iteration count: each attempt can take
    // longer than its own timeout suggests, and "30 tries" silently became
    // minutes of apparent hang when a probe was slow.
    let started = std::time::Instant::now();
    while started.elapsed() < OVERALL_DEADLINE {
        if let Ok(resp) = client
            .get(&probe_url)
            .header("Host", DASHBOARD_HOST)
            .send()
            .await
        {
            if resp.status().is_success() {
                println!(" ready.");
                match portzero_domain::app::launch() {
                    Ok(()) => println!("Opening the PortZero app..."),
                    Err(err) => println!(
                        "Could not open the PortZero app automatically ({err}). \
                         Launch it from your applications menu, or run `portzero start`."
                    ),
                }
                return;
            }
        }
        // Visible progress, so a slow start reads as "working" rather than "hung".
        print!(".");
        let _ = std::io::stdout().flush();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    println!();
    println!(
        "The daemon did not answer within {}s. Setup itself completed — everything above \
         was applied. Check `portzero doctor`; if it reports the overlay is active, you can \
         open the PortZero app from your applications menu or run `portzero start`.",
        OVERALL_DEADLINE.as_secs()
    );
}

/// Install the per-user LaunchAgent that starts `portzero-tray` at login.
///
/// This has to happen here rather than in the Homebrew formula's
/// `post_install`. Homebrew runs `post_install` with `HOME` pointed at a
/// throwaway temp directory, so a formula that writes
/// `~/Library/LaunchAgents/...` silently deposits it in
/// `/private/tmp/portzero-postinstall-*/Library/LaunchAgents/` and brew then
/// deletes it. The formula looked correct and shipped nothing: no tray icon
/// ever appeared for anyone who installed via brew.
///
/// Setup runs as root under `sudo`, so the plist is written into the *real*
/// user's home, chowned to them, and bootstrapped into their GUI session —
/// `launchctl load` as root would target root's session, where there is no GUI.
#[cfg(target_os = "macos")]
fn install_tray_agent() -> Result<()> {
    const LABEL: &str = "cloud.portzero.tray";

    let Some((user, home)) = sudo_invoker() else {
        // Running unprivileged: the current user *is* the target user.
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
        return install_tray_agent_for(&home, None, LABEL);
    };
    install_tray_agent_for(&home, Some(&user), LABEL)
}

#[cfg(target_os = "macos")]
fn install_tray_agent_for(
    home: &std::path::Path,
    chown_to: Option<&str>,
    label: &str,
) -> Result<()> {
    // The tray ships beside this binary in every package we build.
    let tray = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("portzero-tray")))
        .filter(|p| p.exists());
    let Some(tray) = tray else {
        println!("No portzero-tray binary alongside this one; skipping the tray agent.");
        return Ok(());
    };

    let agents_dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents_dir)
        .with_context(|| format!("could not create {}", agents_dir.display()))?;
    let plist_path = agents_dir.join(format!("{label}.plist"));

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array><string>{}</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
"#,
        tray.display()
    );
    std::fs::write(&plist_path, plist)
        .with_context(|| format!("could not write {}", plist_path.display()))?;

    if let Some(user) = chown_to {
        let _ = std::process::Command::new("chown")
            .arg(format!("{user}:"))
            .arg(&plist_path)
            .status();
    }

    // Bootstrap into the target user's GUI domain. Root's own launchctl session
    // has no GUI, so `launchctl load` here would load it nowhere useful.
    if let Some(uid) = target_uid(chown_to) {
        let domain = format!("gui/{uid}");
        // Replace any previous copy; ignore the "not loaded" error on first run.
        let _ = std::process::Command::new("launchctl")
            .args(["bootout", &format!("{domain}/{label}")])
            .status();
        let status = std::process::Command::new("launchctl")
            .args(["bootstrap", &domain])
            .arg(&plist_path)
            .status();
        match status {
            Ok(s) if s.success() => println!("System-tray companion installed and started."),
            _ => println!(
                "Tray agent written to {}. It will start at your next login \
                 (or run `launchctl bootstrap gui/{uid} {}`).",
                plist_path.display(),
                plist_path.display()
            ),
        }
    } else {
        println!("Tray agent written to {}.", plist_path.display());
    }
    Ok(())
}

/// UID of the user whose GUI session should own the tray.
#[cfg(target_os = "macos")]
fn target_uid(user: Option<&str>) -> Option<u32> {
    let out = match user {
        Some(u) => std::process::Command::new("id").args(["-u", u]).output(),
        None => std::process::Command::new("id").arg("-u").output(),
    }
    .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// Give the invoking user back ownership of the directories setup just wrote.
///
/// Setup runs as root (via `sudo`), but every path it touches lives under the
/// *real* user's home — the CA in `~/.local/share/PortZero`, routes and the
/// login token in `~/.portzero`. Without this, `sudo portzero setup` leaves
/// those directories owned by root, and the very next unprivileged command the
/// user runs fails: `portzero login` cannot write `~/.portzero/auth.json`.
/// That made the documented install path (brew caveats say `sudo portzero
/// setup`) produce a CLI that could not log in.
///
/// Root can still write to these paths afterwards, so the daemon is unaffected.
/// No-op when not running under `sudo`.
fn restore_user_ownership() -> Result<()> {
    let Some((user, home)) = sudo_invoker() else {
        println!("Not running under sudo; ownership unchanged.");
        return Ok(());
    };

    let dirs = [home.join(".portzero"), home.join(".local/share/PortZero")];
    let mut restored = Vec::new();
    for dir in dirs.iter().filter(|d| d.exists()) {
        // `chown` resolves the account name itself, which matters on macOS
        // where regular users live in Directory Services, not /etc/passwd.
        let status = std::process::Command::new("chown")
            .arg("-R")
            .arg(format!("{user}:"))
            .arg(dir)
            .status()
            .with_context(|| format!("could not run chown on {}", dir.display()))?;
        if !status.success() {
            anyhow::bail!(
                "chown -R {user}: {} failed ({status}). Run it yourself, or `portzero login` \
                 will not be able to write its token.",
                dir.display()
            );
        }
        restored.push(dir.display().to_string());
    }

    if restored.is_empty() {
        println!("No PortZero directories to reassign.");
    } else {
        println!("Ownership restored to {user}: {}", restored.join(", "));
    }
    Ok(())
}

/// The real user behind `sudo`, and their home directory. `None` when setup was
/// not invoked through `sudo` (so the files already belong to whoever ran it).
fn sudo_invoker() -> Option<(String, std::path::PathBuf)> {
    let user = std::env::var("SUDO_USER").ok()?;
    if user.is_empty() || user == "root" {
        return None;
    }
    // HOME is the reliable source here: macOS `sudo` preserves it, and regular
    // macOS accounts are absent from /etc/passwd.
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from)?;
    if home.as_os_str().is_empty() || home == std::path::Path::new("/var/root") {
        return None;
    }
    Some((user, home))
}

/// Install the scoped `*.portzero.local` OS resolver as part of setup so name
/// resolution works from install time. On macOS this writes
/// `/etc/resolver/portzero.local`; on Linux/Windows the daemon installs the
/// scoped resolver against its TUN link at startup, so this is a no-op.
async fn ensure_scoped_resolver() -> Result<()> {
    portzero_daemon::net::overlay::ensure_scoped_resolver_for_setup()
        .await
        .context(
            "Failed to install the scoped .portzero.local resolver. \
             Re-run setup with administrator privileges.",
        )?;
    println!("Scoped resolver ensured.");
    Ok(())
}

fn ensure_dashboard_hosts_entry(path: &str) -> Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    if has_expected_dashboard_hosts_entry(&content) {
        println!("Hosts entry already present: {DASHBOARD_HOSTS_LINE}");
        return Ok(());
    }

    let safety = portzero_daemon::hosts::check_hosts_write_safety(std::path::Path::new(path));
    if !safety.is_safe() {
        let mut detail = String::new();
        for blocker in &safety.blockers {
            let (label, explanation) = blocker.describe();
            detail.push_str(&format!("\n  - {label}: {explanation}"));
        }
        anyhow::bail!(
            "Refusing to edit {path}: an edit would likely fail or not persist.{detail}\n\
             Add `{DASHBOARD_HOSTS_LINE}` yourself, in whatever way is appropriate for this system."
        );
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| {
            format!("Failed to open {path}. Re-run setup with administrator privileges.")
        })?;

    if !content.is_empty() && !content.ends_with('\n') {
        writeln!(file).with_context(|| format!("Failed to append newline to {path}"))?;
    }
    writeln!(file, "{DASHBOARD_HOSTS_LINE}")
        .with_context(|| format!("Failed to append PortZero hosts entry to {path}"))?;

    println!("Added hosts entry: {DASHBOARD_HOSTS_LINE}");
    Ok(())
}

fn has_expected_dashboard_hosts_entry(content: &str) -> bool {
    content.lines().any(|line| {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            return false;
        }
        let mut fields = line.split_whitespace();
        let Some(ip) = fields.next() else {
            return false;
        };
        ip == DASHBOARD_IP && fields.any(|name| name == DASHBOARD_HOST)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_expected_dashboard_hosts_entry_with_marker() {
        let hosts = "127.0.0.1 localhost\n10.254.0.2 portzero.local # portzero-local\n";
        assert!(has_expected_dashboard_hosts_entry(hosts));
    }

    #[test]
    fn detects_expected_dashboard_hosts_entry_without_marker() {
        let hosts = "10.254.0.2 api.portzero.local portzero.local\n";
        assert!(has_expected_dashboard_hosts_entry(hosts));
    }

    #[test]
    fn ignores_commented_dashboard_hosts_entry() {
        let hosts = "# 10.254.0.2 portzero.local # portzero-local\n";
        assert!(!has_expected_dashboard_hosts_entry(hosts));
    }

    #[test]
    fn rejects_wrong_dashboard_ip() {
        let hosts = "127.0.0.1 portzero.local\n";
        assert!(!has_expected_dashboard_hosts_entry(hosts));
    }

    /// Serializes the tests below, which mutate process-global env vars.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn sudo_invoker_identifies_the_real_user() {
        let _guard = env_lock();
        std::env::set_var("SUDO_USER", "alice");
        std::env::set_var("HOME", "/Users/alice");
        let (user, home) = sudo_invoker().expect("sudo invoker should be detected");
        assert_eq!(user, "alice");
        assert_eq!(home, std::path::Path::new("/Users/alice"));
        std::env::remove_var("SUDO_USER");
    }

    #[test]
    fn sudo_invoker_is_none_without_sudo() {
        let _guard = env_lock();
        std::env::remove_var("SUDO_USER");
        std::env::set_var("HOME", "/Users/alice");
        assert!(sudo_invoker().is_none());
    }

    #[test]
    fn sudo_invoker_is_none_when_root_invoked_directly() {
        let _guard = env_lock();
        // `sudo -u root` (or a root login shell) leaves nothing to hand back.
        std::env::set_var("SUDO_USER", "root");
        std::env::set_var("HOME", "/Users/alice");
        assert!(sudo_invoker().is_none());
        std::env::remove_var("SUDO_USER");
    }

    #[test]
    fn sudo_invoker_rejects_roots_home() {
        let _guard = env_lock();
        // Without HOME preservation we would otherwise chown /var/root.
        std::env::set_var("SUDO_USER", "alice");
        std::env::set_var("HOME", "/var/root");
        assert!(sudo_invoker().is_none());
        std::env::remove_var("SUDO_USER");
    }
}
