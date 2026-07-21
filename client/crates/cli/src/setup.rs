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

/// Wait until the daemon answers over its real DNS path (up to ~30s), then
/// launch the PortZero desktop app. Best-effort: prints a hint and returns
/// rather than failing setup if the daemon never becomes reachable (or the app
/// can't be launched). The readiness probe bypasses any proxy so a corporate
/// `HTTP_PROXY` can't swallow the local request.
async fn wait_and_open_app() {
    const PROBE_URL: &str = "http://portzero.local/status.json";

    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    print!("Waiting for the daemon to become ready...");
    let _ = std::io::stdout().flush();
    for _ in 0..30 {
        if let Ok(resp) = client.get(PROBE_URL).send().await {
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
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    println!();
    println!(
        "The daemon is not ready yet. Once `portzero status` shows it running, \
         open the PortZero app from your applications menu or run `portzero start`."
    );
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
}
