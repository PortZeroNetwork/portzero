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
use portzero_daemon::management::pid_lookup::pid_is_alive;

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
    acquire_at(&pid_path(&config))
}

/// Same as [`acquire`], but against an explicit PID file path — split out so
/// tests can point it at a scratch directory instead of the real `~/.portzero`.
fn acquire_at(path: &Path) -> Result<(), u32> {
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
                match existing_owner(path) {
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

/// If `path` names a live process, return its PID. If it names a dead one,
/// remove the stale file and return `None`.
fn existing_owner(path: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;
    if pid_is_alive(pid) {
        Some(pid)
    } else {
        let _ = std::fs::remove_file(path);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_instance_claims_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");

        assert!(acquire_at(&path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn second_instance_is_refused_while_first_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");

        // Our own PID is always alive, so writing it directly simulates a
        // live "other" instance without needing to spawn a real process.
        std::fs::write(&path, std::process::id().to_string()).unwrap();

        assert_eq!(acquire_at(&path), Err(std::process::id()));
    }

    #[test]
    fn stale_lock_from_a_dead_process_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tray.pid");

        // PID 0 is never a real, live process on any platform this runs on.
        std::fs::write(&path, "0").unwrap();

        assert!(acquire_at(&path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn creates_missing_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("tray.pid");

        assert!(acquire_at(&path).is_ok());
        assert!(path.exists());
    }
}
