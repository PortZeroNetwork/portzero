//! Real-subprocess tests for `portzero status` / `stop` / `restart` (daemon
//! lifecycle commands in `src/daemon.rs`).
//!
//! Follows the pattern established by `tests/signal_shutdown.rs`: spawn the
//! actual built binary (`env!("CARGO_BIN_EXE_portzero")`) under a throwaway
//! `HOME` so state files (`~/.portzero/...`) never touch the real user
//! environment, and clean up afterward.
//!
//! Coverage note: the discovery daemon writes its PID file and becomes
//! visible to `status`/`stop` very early in `run_discovery_loop` — before it
//! attempts to bring up the TUN overlay device — so `start --foreground`,
//! `status`, `stop`, and `restart` are all reachable unprivileged in this
//! sandbox (it just degrades to local/cloud-only with no TUN, same as
//! `signal_shutdown.rs` relies on). What is NOT covered here: the overlay
//! network actually coming up (CAP_NET_ADMIN/root), and cloud-connected
//! states (`status` reporting a real edge connection) — both require real
//! privilege/network setup outside a CI sandbox and are exercised, if at
//! all, by the staging E2E suite instead.
//!
//! Unix-only, same as `signal_shutdown.rs`: `DaemonConfig::default()` locates
//! the state dir via `dirs::home_dir()`, which on Windows calls
//! `SHGetKnownFolderPath` directly and ignores the `HOME` env var, so the
//! throwaway-`HOME` isolation below is a no-op there — every subprocess would
//! share the runner's real `~/.portzero/daemon/daemon.pid`, and parallel test
//! threads stomp on each other's PID files.

#![cfg(unix)]

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A throwaway `$HOME` for one test, cleaned up on drop.
struct TempHome {
    path: std::path::PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "portzero-daemon-lifecycle-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create temp home");
        Self { path }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_portzero")
}

/// Run `portzero <args>` to completion against `home`, capturing output.
fn run(home: &TempHome, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .env("HOME", &home.path)
        .env("RUST_LOG", "error")
        .output()
        .expect("spawn portzero subcommand")
}

/// Path to the PID file `daemon.rs`/`discovery_loop.rs` write to under `home`.
fn pid_file_path(home: &TempHome) -> std::path::PathBuf {
    home.path
        .join(".portzero")
        .join("daemon")
        .join("daemon.pid")
}

/// Poll for the PID file to appear (and contain a parseable PID) within
/// `deadline`, returning it once found.
fn wait_for_pid_file(home: &TempHome, deadline: Duration) -> Option<u32> {
    let start = Instant::now();
    let path = pid_file_path(home);
    while start.elapsed() < deadline {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(pid) = content.trim().parse::<u32>() {
                return Some(pid);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// Wait for a spawned child to exit within `deadline`; force-kill and reap it
/// if it doesn't, so no test ever leaks an orphan process.
fn wait_for_exit_or_kill(
    child: &mut Child,
    deadline: Duration,
) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    let mut exited = None;
    while start.elapsed() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait") {
            exited = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if exited.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    exited
}

/// `portzero status` when no daemon is running: should exit 0 and say so,
/// without erroring — this is the common case for a fresh checkout / CI job
/// that hasn't started the daemon yet.
#[test]
fn status_reports_stopped_when_no_daemon_running() {
    let home = TempHome::new("status-stopped");
    let output = run(&home, &["status"]);

    assert!(
        output.status.success(),
        "status should exit 0 even when the daemon is stopped: {:?}",
        output
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Daemon: stopped"), "stdout was: {stdout}");
    assert!(
        stdout.contains("portzero start"),
        "expected a hint to run `portzero start`, stdout was: {stdout}"
    );
}

/// `portzero stop` when no daemon is running: should fail gracefully (a clear
/// non-zero exit with an actionable message on stderr), not panic or hang.
#[test]
fn stop_fails_gracefully_when_no_daemon_running() {
    let home = TempHome::new("stop-not-running");
    let output = run(&home, &["stop"]);

    assert!(
        !output.status.success(),
        "stop should exit non-zero when nothing is running: {:?}",
        output
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Discovery daemon is not running"),
        "stderr was: {stderr}"
    );
    assert!(
        stderr.contains("portzero start"),
        "expected a hint to run `portzero start`, stderr was: {stderr}"
    );
}

/// Full reachable lifecycle: start a foreground daemon, confirm `status` sees
/// it running, `stop` it, and confirm `status` reports it stopped again and
/// the foreground process actually exited.
#[test]
fn status_stop_status_cycle_against_a_real_foreground_daemon() {
    let home = TempHome::new("lifecycle");

    let mut daemon = Command::new(bin())
        .args(["start", "--foreground"])
        .env("HOME", &home.path)
        .env("RUST_LOG", "error")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn foreground daemon");

    let pid = wait_for_pid_file(&home, Duration::from_secs(10));
    if pid.is_none() {
        // Daemon never claimed its PID file — nothing more to assert; make
        // sure we don't leak the child either way.
        wait_for_exit_or_kill(&mut daemon, Duration::from_secs(2));
        panic!("daemon did not write its PID file within 10s");
    }
    let pid = pid.unwrap();

    // It must still be alive before we probe it.
    if let Some(status) = daemon.try_wait().expect("try_wait") {
        panic!("daemon exited before status/stop could run (status {status:?})");
    }

    // `status` should now report it running with the correct PID.
    let status_output = run(&home, &["status"]);
    assert!(
        status_output.status.success(),
        "status while running should exit 0: {:?}",
        status_output
    );
    let stdout = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        stdout.contains(&format!("Daemon: running (PID {pid})")),
        "stdout was: {stdout}"
    );

    // `stop` should succeed and actually terminate the foreground process.
    let stop_output = run(&home, &["stop"]);
    assert!(
        stop_output.status.success(),
        "stop should exit 0 when the daemon is running: {:?}",
        stop_output
    );
    let stop_stdout = String::from_utf8_lossy(&stop_output.stdout);
    assert!(
        stop_stdout.contains("Daemon stopped."),
        "stdout was: {stop_stdout}"
    );

    let exited = wait_for_exit_or_kill(&mut daemon, Duration::from_secs(8));
    assert!(
        exited.is_some(),
        "foreground daemon did not exit within 8s after `portzero stop`"
    );

    // `status` should report it stopped again.
    let final_status = run(&home, &["status"]);
    assert!(final_status.status.success());
    let final_stdout = String::from_utf8_lossy(&final_status.stdout);
    assert!(
        final_stdout.contains("Daemon: stopped"),
        "stdout was: {final_stdout}"
    );
}

/// `portzero restart` against a running foreground daemon: it should stop the
/// existing one and spawn a fresh (background, detached) daemon that claims a
/// new PID. This exercises the real stop+respawn path end to end; the new
/// background process is cleaned up via a follow-up `portzero stop` plus a
/// belt-and-braces `kill` at the end of the test.
#[test]
fn restart_stops_old_daemon_and_spawns_a_new_one() {
    let home = TempHome::new("restart");

    let mut daemon = Command::new(bin())
        .args(["start", "--foreground"])
        .env("HOME", &home.path)
        .env("RUST_LOG", "error")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn foreground daemon");

    let original_pid = wait_for_pid_file(&home, Duration::from_secs(10));
    if original_pid.is_none() {
        wait_for_exit_or_kill(&mut daemon, Duration::from_secs(2));
        panic!("daemon did not write its PID file within 10s");
    }
    let original_pid = original_pid.unwrap();

    // `restart` re-invokes the built binary itself (`current_exe()` +
    // `start --foreground`) as a *detached* background process, so it is not
    // a child of this test and won't be reaped by wait_for_exit_or_kill.
    let restart_output = run(&home, &["restart"]);

    // The original foreground daemon should have been asked to exit as part
    // of restart's stop-then-start.
    let original_exited = wait_for_exit_or_kill(&mut daemon, Duration::from_secs(8));
    assert!(
        original_exited.is_some(),
        "original foreground daemon did not exit after `portzero restart`"
    );

    // Give the new background daemon a moment past `restart`'s own internal
    // wait to (re)claim the PID file with a different PID.
    let new_pid = wait_for_pid_file(&home, Duration::from_secs(5));

    // Always try to stop whatever ended up running before asserting, so a
    // failed assertion doesn't leak a background process.
    let cleanup_result = run(&home, &["stop"]);

    if let Some(new_pid) = new_pid {
        assert_ne!(
            new_pid, original_pid,
            "restart should spawn a daemon with a new PID, not reuse the old one"
        );
        assert!(
            cleanup_result.status.success(),
            "expected `portzero stop` to be able to stop the restarted daemon: {:?}",
            cleanup_result
        );
    } else {
        // Surface restart's own stdout/stderr for debugging rather than
        // silently passing when the new daemon never came up.
        panic!(
            "no PID file present after `portzero restart`; restart stdout/stderr: {:?}",
            restart_output
        );
    }

    // Belt-and-braces: if a PID somehow escaped `portzero stop` (e.g. it
    // hadn't finished starting up when `stop` ran), make sure it's not left
    // behind for the rest of the test suite / CI runner.
    if let Some(pid) = new_pid {
        #[cfg(unix)]
        {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg(pid.to_string())
                .status();
        }
    }
}
