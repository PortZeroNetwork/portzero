//! Runtime signal-handling test for the foreground daemon (task-32).
//!
//! Regression guard for the bug where `port-zero start --foreground` did
//! not stop on Ctrl-C / SIGTERM (it had to be `pkill`'d). The daemon runs fine
//! unprivileged — it just degrades to local/cloud-only with no TUN — so we can
//! exercise the real signal path end-to-end without root.
//!
//! Each case spawns the actual built binary under a throwaway `HOME` (so it
//! never touches a real pid file), lets it come up, sends the signal, and
//! asserts the process exits promptly. Only the child we spawned is ever
//! signalled.

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Send `signal` (e.g. "INT", "TERM") to `pid` via the system `kill`.
fn send_signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .expect("failed to invoke kill");
    assert!(status.success(), "kill -{signal} {pid} failed");
}

/// Spawn the foreground daemon, send `signal` after it has started, and assert
/// it exits within `deadline`.
fn assert_exits_on_signal(signal: &str) {
    let bin = env!("CARGO_BIN_EXE_portzero");
    let tmp_home = std::env::temp_dir().join(format!(
        "portzero-sigtest-{}-{}",
        signal,
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp_home).expect("create temp home");

    let mut child = Command::new(bin)
        .args(["start", "--foreground"])
        .env("HOME", &tmp_home)
        .env("RUST_LOG", "info")
        // Discard output so a full pipe buffer can never wedge the child.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    // Give the daemon time to start its scan loop (scan_interval is 2s, so this
    // lands us mid-cycle — exactly the window the old code could not interrupt).
    std::thread::sleep(Duration::from_secs(3));

    // It must still be running before we signal it.
    if let Some(status) = child.try_wait().expect("try_wait") {
        let _ = std::fs::remove_dir_all(&tmp_home);
        panic!("daemon exited before we signalled it (status {status:?})");
    }

    let pid = child.id();
    send_signal(pid, signal);

    let deadline = Duration::from_secs(8);
    let start = Instant::now();
    let mut exited = None;
    while start.elapsed() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait") {
            exited = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let elapsed = start.elapsed();

    if exited.is_none() {
        // Don't leave a zombie/orphan around.
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_dir_all(&tmp_home);

    assert!(
        exited.is_some(),
        "daemon did not exit within {deadline:?} after SIG{signal}"
    );
    // Sanity: should be fast (well under the 5s graceful-shutdown watchdog).
    assert!(
        elapsed < deadline,
        "daemon took too long ({elapsed:?}) to exit after SIG{signal}"
    );
    eprintln!("SIG{signal}: daemon exited in {elapsed:?}");
}

#[test]
fn foreground_daemon_stops_on_sigint() {
    assert_exits_on_signal("INT");
}

#[test]
fn foreground_daemon_stops_on_sigterm() {
    assert_exits_on_signal("TERM");
}
