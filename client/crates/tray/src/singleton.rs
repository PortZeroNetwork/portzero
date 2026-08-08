//! Single-instance guard for the tray companion process.
//!
//! The tray can be launched by more than one path at once — login autostart,
//! the package post-install "get it running now" launch (see
//! `packaging/linux/deb/postinst`), or a stale process left over from a
//! package upgrade — and none of those paths know about each other. Without a
//! guard, two `portzero-tray` processes each register their own tray icon,
//! which is what produced the duplicate icon reported on Linux.
//!
//! Unlike the daemon's `acquire_singleton_or_take_over`
//! (`portzero_daemon::discovery_loop`), the tray never signals or kills the
//! other instance: it holds no state worth taking over, so the simplest safe
//! fix is to just decline to start a second one and let the existing tray
//! keep running.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use portzero_daemon::discovery_loop::DaemonConfig;
use portzero_daemon::management::pid_lookup::process_is_alive_named;
use portzero_daemon::versions::Component;

/// `~/.portzero/tray.pid` (sits next to the daemon state dir, like the
/// welcome marker in `welcome.rs`).
fn pid_path(config: &DaemonConfig) -> PathBuf {
    config
        .state_dir
        .parent()
        .map(|p| p.join("tray.pid"))
        .unwrap_or_else(|| PathBuf::from(".portzero/tray.pid"))
}

/// Claim the tray singleton for this process.
///
/// Returns `Ok(())` once this process owns the PID file, or `Err(pid)` with
/// the PID of the tray that already holds it.
pub fn acquire() -> Result<(), u32> {
    let config = DaemonConfig::load();
    acquire_at(&pid_path(&config), Component::Tray.binary_stem())
}

/// Same as [`acquire`], but against an explicit PID file path and expected
/// owner binary — split out so tests can point it at a scratch directory
/// instead of the real `~/.portzero`, and name the test binary as the owner
/// they are simulating.
fn acquire_at(path: &Path, expected_stem: &str) -> Result<(), u32> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Loop at most twice: once for the common case (no stale file, or a live
    // owner), and once more if the first pass found a stale file and cleared
    // it — after which creation should succeed.
    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                // Best-effort: if the write fails partway, a corrupt/short PID
                // file just reads back as "no valid owner" next time, which
                // is the safe direction to fail in.
                let _ = write!(file, "{}", std::process::id());
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match existing_owner(path, expected_stem) {
                    Some(pid) => return Err(pid),
                    // Stale file already removed by `existing_owner`; retry
                    // the atomic create.
                    None => continue,
                }
            }
            Err(_) => {
                // Can't create the PID file at all (e.g. unwritable state
                // dir) — proceed rather than block the tray from starting at
                // all over a filesystem issue unrelated to duplication.
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Drop this process's claim on the tray singleton, on a clean exit.
///
/// Only removes the file if we still own it, so a tray shutting down late
/// cannot delete a lock a newer tray has since taken. Releasing is best-effort
/// housekeeping, not correctness: [`existing_owner`] has to survive the
/// crash/kill/session-teardown paths that never reach here, and does.
pub fn release() {
    let config = DaemonConfig::load();
    release_at(&pid_path(&config), std::process::id());
}

fn release_at(path: &Path, owner: u32) {
    let held_by_us = std::fs::read_to_string(path)
        .ok()
        .and_then(|c| c.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid == owner);
    if held_by_us {
        let _ = std::fs::remove_file(path);
    }
}

/// If `path` names a live tray process, return its PID. Otherwise remove the
/// stale file and return `None`.
///
/// The owner must still be running `portzero-tray` specifically, not merely be
/// a live PID. PIDs are recycled, and a tray that exits without clearing this
/// file (killed, or torn down with the desktop session) leaves a number behind
/// that some unrelated process eventually takes — after which a PID-only check
/// declares the tray "already running" at every login and the user never gets a
/// tray icon again. That was not hypothetical: on a KDE machine the tray's old
/// PID was reused by a `kaccess` thread and the tray stayed unstartable across
/// reboots.
fn existing_owner(path: &Path, expected_stem: &str) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;
    if process_is_alive_named(pid, expected_stem) {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(path);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portzero_daemon::management::pid_lookup::running_binary_stem;

    /// The binary these tests run as, so a test can write its own live PID into
    /// the lock file and have it accepted as a genuine owner.
    fn own_stem() -> String {
        running_binary_stem(std::process::id())
            .expect("the test binary must be identifiable for these tests to mean anything")
    }

    #[test]
    fn first_instance_claims_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");

        assert!(acquire_at(&path, &own_stem()).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn second_instance_is_refused_while_first_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");

        // Our own PID is always alive and running our own binary, so writing it
        // directly simulates a live "other" instance without spawning a real
        // process.
        std::fs::write(&path, std::process::id().to_string()).unwrap();

        assert_eq!(acquire_at(&path, &own_stem()), Err(std::process::id()));
    }

    #[test]
    fn stale_lock_from_a_dead_process_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");

        // PID 0 is never a real, live process on any platform this runs on.
        std::fs::write(&path, "0").unwrap();

        assert!(acquire_at(&path, &own_stem()).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );
    }

    /// The regression that left a machine with no tray icon: the tray exited
    /// without clearing the lock, and its PID was later recycled by an
    /// unrelated process. A liveness-only check called that "already running"
    /// and refused to start a tray at every login, permanently.
    #[test]
    fn lock_held_by_a_recycled_pid_running_another_binary_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");

        // A live PID (our own) that is *not* a tray — exactly what a recycled
        // PID looks like.
        std::fs::write(&path, std::process::id().to_string()).unwrap();

        assert!(
            acquire_at(&path, "portzero-tray").is_ok(),
            "a live PID running some other binary must not hold the tray lock"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn release_clears_a_lock_we_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");

        assert!(acquire_at(&path, &own_stem()).is_ok());
        release_at(&path, std::process::id());
        assert!(!path.exists(), "our own lock should be released");
    }

    /// A tray shutting down slowly must not delete the lock a newer tray took
    /// after it was reclaimed as stale.
    #[test]
    fn release_leaves_a_lock_owned_by_someone_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");
        std::fs::write(&path, "4242").unwrap();

        release_at(&path, std::process::id());
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "4242");
    }

    #[test]
    fn creates_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("tray.pid");

        assert!(acquire_at(&path, &own_stem()).is_ok());
        assert!(path.exists());
    }
}
