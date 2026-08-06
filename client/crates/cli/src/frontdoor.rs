//! Bare `portzero`: a friendly front door instead of a usage error.
//!
//! Prints a short, state-aware summary (daemon, auth, route count — the same
//! state `portzero status` reads) and exactly one next-step line appropriate
//! to that state, at exit code 0.

use portzero_daemon::discovery_loop::{read_daemon_pid, DaemonConfig};

use crate::auth::AuthConfig;
use crate::export;

/// Print the front-door summary. Never fails: every read is best-effort so a
/// fresh install with no state at all still gets a useful answer.
pub fn run() {
    let config = DaemonConfig::load();
    let daemon_pid = read_daemon_pid(&config);
    let tunnels = export::discovered_tunnels(&config);
    let set_up = config.state_dir.exists();

    println!("Port Zero — stable *.portzero.local names for your dev processes and containers");
    println!();
    match daemon_pid {
        Some(pid) => println!("Daemon: running (PID {pid})"),
        None => println!("Daemon: stopped"),
    }
    match AuthConfig::load() {
        Ok(auth) => println!("Auth:   logged in as {}", auth.email),
        Err(_) => println!("Auth:   not logged in (Local tunnels work without an account)"),
    }
    match tunnels.len() {
        0 => println!("Routes: none discovered yet"),
        n => println!("Routes: {n} tunnel(s) discovered"),
    }
    println!();
    println!(
        "Next:   {}",
        next_step(set_up, daemon_pid.is_some(), tunnels.len())
    );

    // With nothing registered, the missing piece is almost never a command —
    // it is knowing that PZ_TUNNEL is the whole registration story.
    if tunnels.is_empty() {
        println!();
        println!("{}", crate::pz_tunnel_contract());
    }

    println!();
    println!("Run `portzero --help` for all commands.");
}

/// The single most useful next step for the current state.
fn next_step(set_up: bool, daemon_running: bool, route_count: usize) -> &'static str {
    if !set_up {
        "run `sudo portzero setup` to finish first-run setup, then `portzero demo`"
    } else if !daemon_running {
        "run `portzero start` to start the daemon"
    } else if route_count == 0 {
        "run `portzero demo` to see a working Local tunnel, or set PZ_TUNNEL on your own process"
    } else {
        "run `portzero status` for tunnel details"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_points_at_setup() {
        assert!(next_step(false, false, 0).contains("portzero setup"));
    }

    #[test]
    fn stopped_daemon_points_at_start() {
        assert!(next_step(true, false, 0).contains("portzero start"));
    }

    #[test]
    fn running_with_no_routes_points_at_demo_and_pz_tunnel() {
        let step = next_step(true, true, 0);
        assert!(step.contains("portzero demo"));
        assert!(step.contains("PZ_TUNNEL"));
    }

    #[test]
    fn live_routes_point_at_status() {
        assert!(next_step(true, true, 3).contains("portzero status"));
    }
}
