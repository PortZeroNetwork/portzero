//! End-to-end parsing of the daemon's real on-disk JSON formats into a tray
//! [`Snapshot`]. These guard against drift between the daemon's serialized state
//! and what the tray expects to read — the failure mode where the tray silently
//! shows an empty menu because a format changed under it.

use std::fs;
use std::path::Path;

use portzero_daemon::discovery_loop::DaemonConfig;
use portzero_daemon::net::stack::OverlayHttpsPolicy;
use portzero_tray::state::{Health, Snapshot};

fn write(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

/// A config pointed at `state_dir`, with an explicit HTTPS policy so the result
/// never depends on the developer's real `~/.portzero/config.toml`.
fn config_for(state_dir: &Path, enable_https: bool) -> DaemonConfig {
    DaemonConfig {
        state_dir: state_dir.to_path_buf(),
        overlay_https: OverlayHttpsPolicy {
            enable_for_port_80: enable_https,
            redirect_port_80: true,
            passthrough_port_443: true,
        },
        ..Default::default()
    }
}

#[test]
fn reads_running_daemon_with_tunnels_and_issues() {
    let tmp = tempfile::tempdir().unwrap();
    let sd = tmp.path();

    // Our own pid is guaranteed alive, so the daemon reads as "running".
    write(sd, "daemon.pid", &std::process::id().to_string());
    write(
        sd,
        "overlay.json",
        r#"{ "overlay_active": true, "routes": [
          { "domain": "web.myapp.portzero.local", "service_port": 80, "real_addr": "127.0.0.1:33001", "pid": 111, "source": { "type": "Process", "cwd": "/home/user/app" } },
          { "domain": "api.myapp.portzero.local", "service_port": 443, "real_addr": "127.0.0.1:33002", "pid": 112, "source": { "type": "Process", "cwd": "/home/user/app" } }
        ] }"#,
    );
    write(
        sd,
        "routes.json",
        r#"{ "routes": { "api.alice.tunnel.portzero.cloud": {
          "domain": "api.alice.tunnel.portzero.cloud", "host": "127.0.0.1", "port": 8080,
          "source": { "type": "Process", "cwd": "/home/user/app" }, "pid": 113,
          "discovered_at": "2026-07-11T00:00:00Z" } } }"#,
    );
    write(
        sd,
        "issues.json",
        r#"{ "issues": [ { "kind": "DuplicateName", "name": "db", "claimants": ["pid 1 (~/a)", "pid 2 (~/b)"] } ] }"#,
    );
    write(
        sd,
        "diagnostics.json",
        r#"{ "generated_at": "2026-07-11T00:00:00Z", "checks_run": 5, "issues": [
          { "id": "tunnel_dns_failed", "severity": "warning", "category": "dns",
            "title": "web.myapp.portzero.local is not resolving", "detail": "no answer",
            "fix": { "kind": "manual", "description": "Run portzero doctor" } },
          { "id": "dns_probe_ok", "severity": "info", "category": "dns",
            "title": "portzero.local resolution works", "detail": "ok" } ] }"#,
    );

    let snap = Snapshot::read(&config_for(sd, false));

    assert!(snap.running, "live pid should read as running");

    // 2 local + 1 cloud tunnel, all parsed.
    assert_eq!(snap.tunnels.len(), 3, "expected 2 local + 1 cloud tunnel");

    let web = snap
        .tunnels
        .iter()
        .find(|t| t.domain == "web.myapp.portzero.local")
        .expect("web tunnel present");
    assert!(!web.https, "port 80 with https disabled is http");
    assert_eq!(web.url, "http://web.myapp.portzero.local");

    let api = snap
        .tunnels
        .iter()
        .find(|t| t.domain == "api.myapp.portzero.local")
        .expect("api tunnel present");
    assert!(api.https, "port 443 is https");

    let cloud = snap
        .tunnels
        .iter()
        .find(|t| t.cloud)
        .expect("cloud tunnel present");
    assert!(cloud.https);
    assert_eq!(cloud.url, "https://api.alice.tunnel.portzero.cloud");

    // The duplicate-name issue (issues.json) and the DNS warning (diagnostics.json)
    // both surface; the informational diagnostic is filtered out.
    assert!(
        snap.problems.iter().any(|p| p.title.contains("db")),
        "duplicate-name issue should surface"
    );
    assert!(
        snap.problems
            .iter()
            .any(|p| p.title.contains("not resolving")),
        "tunnel DNS warning should surface"
    );
    assert!(
        !snap
            .problems
            .iter()
            .any(|p| p.title.contains("resolution works")),
        "informational diagnostics must be filtered out"
    );

    // Running + a warning (no critical) → degraded.
    assert_eq!(snap.health, Health::Degraded);
}

#[test]
fn https_enabled_makes_port_80_tunnels_https() {
    let tmp = tempfile::tempdir().unwrap();
    let sd = tmp.path();
    write(sd, "daemon.pid", &std::process::id().to_string());
    write(
        sd,
        "overlay.json",
        r#"{ "overlay_active": true, "routes": [
          { "domain": "web.myapp.portzero.local", "service_port": 80, "real_addr": "127.0.0.1:33001", "pid": 111, "source": { "type": "Process", "cwd": "/x" } } ] }"#,
    );

    let snap = Snapshot::read(&config_for(sd, true));
    let web = &snap.tunnels[0];
    assert!(
        web.https,
        "port 80 tunnel is https when the policy enables it"
    );
    assert_eq!(web.url, "https://web.myapp.portzero.local");
}

#[test]
fn reports_down_when_pid_is_dead() {
    let tmp = tempfile::tempdir().unwrap();
    let sd = tmp.path();
    // A pid that is almost certainly not alive.
    write(sd, "daemon.pid", "2147483646");

    let snap = Snapshot::read(&config_for(sd, false));
    assert!(!snap.running);
    assert_eq!(snap.health, Health::Down);
}

#[test]
fn empty_state_dir_reads_as_down_and_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = Snapshot::read(&config_for(tmp.path(), false));
    assert!(!snap.running);
    assert!(snap.tunnels.is_empty());
    assert_eq!(snap.health, Health::Down);
}
