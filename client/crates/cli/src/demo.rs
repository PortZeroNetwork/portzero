//! `portzero demo`: one command from fresh install to a working Local tunnel.
//!
//! Ensures the daemon is running, spawns the hidden `portzero demo-server`
//! (a std-only web server that binds port 0) with `PZ_TUNNEL` set in its
//! environment at spawn time — exactly the mechanism users apply to their own
//! processes — waits for the route to become reachable, and opens the page.
//! On failure it runs the two doctor checks behind the #1 first-run failure
//! and prints their fix lines instead of a bare error.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

use portzero_daemon::discovery_loop::{read_daemon_pid, DaemonConfig};
use portzero_daemon::route_table::OverlayState;

use crate::{browser, daemon, doctor, export, wait};

/// The Local tunnel domain the demo claims.
const DEMO_DOMAIN: &str = "hello.portzero.local";
/// The full `PZ_TUNNEL` value set on the demo server (public port 80 so the
/// URL needs no explicit port).
const DEMO_PZ_TUNNEL: &str = "hello.portzero.local:80";
/// How long to wait for the demo tunnel to be discovered and reachable.
const DEMO_TIMEOUT_SECS: u64 = 30;

/// `portzero demo [--no-browser]`.
pub async fn run(no_browser: bool) -> Result<()> {
    let config = DaemonConfig::load();
    ensure_daemon_running(&config)?;

    let mut server = spawn_demo_server()?;
    println!(
        "Demo server started (PID {}) with PZ_TUNNEL={DEMO_PZ_TUNNEL} in its environment.",
        server.id()
    );
    println!("Waiting for the daemon to discover and route it (up to {DEMO_TIMEOUT_SECS}s)...");

    // Reuse `portzero wait`'s readiness logic; `healthy` polls the page over
    // the tunnel itself, so success means truly reachable end to end.
    if let Err(err) = wait::wait(DEMO_DOMAIN, true, Some(DEMO_TIMEOUT_SECS)).await {
        let _ = server.kill();
        let _ = server.wait();
        return report_demo_failure(&config, err).await;
    }

    // Resolve the URL through the same logic `portzero url` uses.
    let url = export::lookup_tunnel(&config, DEMO_DOMAIN)
        .map(|t| t.url)
        .unwrap_or_else(|| format!("http://{DEMO_DOMAIN}"));

    print_success(&url, no_browser);
    wait_for_shutdown(&mut server).await;
    let _ = server.kill();
    let _ = server.wait();
    println!("Demo stopped. The tunnel and its OS-assigned port are gone.");
    Ok(())
}

/// Make sure the daemon is up, starting it (without launching the desktop
/// app) if needed. Failure messages carry the same guidance `portzero start`
/// and `portzero doctor` give.
fn ensure_daemon_running(config: &DaemonConfig) -> Result<()> {
    if read_daemon_pid(config).is_some() {
        return Ok(());
    }
    println!("The daemon is not running yet — starting it...");
    let spawned = daemon::spawn_daemon(config)?;
    if spawned.confirmed {
        println!("Daemon started (PID {}).", spawned.pid);
        return Ok(());
    }
    anyhow::bail!(
        "The daemon was spawned (PID {}) but did not confirm startup.\n\n\
         Next steps:\n\
         - check the daemon log for errors: {}\n\
         - run `portzero doctor` for a full diagnosis, then `portzero demo` again",
        spawned.pid,
        spawned.log_path.display()
    )
}

/// Spawn the hidden `portzero demo-server` child. `PZ_TUNNEL` must be in the
/// child's environment at spawn time — the daemon discovers tunnels by
/// reading the process environment, exactly as it will for the user's own
/// dev command. Stdin is a pipe the child watches: when this process exits,
/// the pipe closes and the server shuts itself down instead of lingering.
fn spawn_demo_server() -> Result<Child> {
    let exe = std::env::current_exe().context(
        "Could not determine the path to the portzero binary.\n\n\
         Try running with an absolute path, e.g. /usr/local/bin/portzero demo",
    )?;
    Command::new(&exe)
        .arg("demo-server")
        .env("PZ_TUNNEL", DEMO_PZ_TUNNEL)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "Failed to spawn the demo server from: {}\n\n\
                 Is the portzero binary executable?",
                exe.display()
            )
        })
}

/// Print the success epilogue: the URL, what just happened, and how to tag
/// the user's own process. Opens the browser unless `--no-browser` was given.
fn print_success(url: &str, no_browser: bool) {
    println!();
    println!("Your first Local tunnel is live: {url}");
    if no_browser {
        println!("Open it in your browser to see the page (--no-browser given).");
    } else if browser::open_browser(url) {
        println!("Opening it in your browser...");
    } else {
        println!("Could not open a browser automatically — open {url} yourself.");
    }
    println!();
    println!("What just happened:");
    println!("  - the demo server bound port 0, so the OS picked a free port (no conflicts)");
    println!("  - it carried PZ_TUNNEL={DEMO_PZ_TUNNEL} in its environment");
    println!("  - the daemon discovered it and routed {url} to it");
    println!();
    println!("This is a Local tunnel — free, local-only, no account needed. Tag your own");
    println!("process or Docker container the same way (set its port to 0 first):");
    println!();
    println!("  PZ_TUNNEL=web.myapp.portzero.local:80 <your dev command>");
    println!();
    println!("More examples: https://portzero.net/docs/examples");
    println!();
    println!("The demo keeps running so you can play with the page. Press Ctrl+C to stop.");
}

/// Block until Ctrl+C (the same tokio `ctrl_c` idiom the daemon's lifecycle
/// uses), or until the demo server exits on its own.
async fn wait_for_shutdown(server: &mut Child) {
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!();
                println!("Stopping the demo...");
                return;
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if let Ok(Some(status)) = server.try_wait() {
                    println!("The demo server exited unexpectedly ({status}).");
                    println!(
                        "Re-run `portzero demo`; if it keeps happening, run `portzero doctor`."
                    );
                    return;
                }
            }
        }
    }
}

/// The demo tunnel never became reachable. Per the user-facing-errors policy,
/// don't just error: run the two doctor checks behind the #1 first-run
/// failure (overlay active, `.portzero.local` OS resolution), print their
/// per-OS `fix:` lines, and point at the full doctor.
async fn report_demo_failure(config: &DaemonConfig, err: anyhow::Error) -> Result<()> {
    println!();
    println!("The demo tunnel did not come up: {err}");
    println!();
    println!("Checking the usual first-run causes:");
    let overlay = OverlayState::load(&config.overlay_path()).unwrap_or_default();
    let pid = read_daemon_pid(config);
    doctor::print_alert(&doctor::check_overlay_active(pid, &overlay));
    doctor::print_alert(&doctor::check_os_resolution().await);
    println!();
    println!("Run `portzero doctor` for the full diagnosis. Once the failing check is");
    println!("fixed, run `portzero demo` again.");
    anyhow::bail!("the demo tunnel did not become reachable within {DEMO_TIMEOUT_SECS}s")
}
