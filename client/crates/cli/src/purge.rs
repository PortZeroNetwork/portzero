//! `portzero purge`: erase local personal data under `~/.portzero/`.
//!
//! `portzero logout` removes only `~/.portzero/auth.json`. This command removes
//! everything the client stores that could be considered personal: credentials,
//! agent-session cost metadata, observed tunnel edges, the daemon log, and the
//! daemon's runtime state files. It never touches Port Zero's servers.
//!
//! See `docs/users/privacy.md` for the full inventory of what the client stores
//! (the canonical, published copy lives at portzero.net/docs).

use std::path::PathBuf;

use anyhow::Result;

use portzero_daemon::discovery_loop::{read_daemon_pid, DaemonConfig};

use crate::auth::AuthConfig;
use crate::cost_store;

/// One local file that may hold personal data, with a human-readable label.
struct PurgeTarget {
    path: PathBuf,
    label: &'static str,
}

/// The full set of local personal-data files `purge` removes.
///
/// Kept as an explicit, enumerated list (not a recursive directory wipe) so the
/// command never deletes beyond what is documented in the privacy notice.
fn purge_targets(config: &DaemonConfig) -> Result<Vec<PurgeTarget>> {
    let mut targets = vec![
        PurgeTarget {
            path: AuthConfig::path()?,
            label: "account credentials",
        },
        PurgeTarget {
            path: cost_store::store_path()?,
            label: "agent-session cost metadata",
        },
    ];

    // Daemon runtime state under ~/.portzero/daemon/.
    let daemon_files: [(PathBuf, &'static str); 10] = [
        (config.observations_path(), "observed tunnel edges/routes"),
        (config.log_path(), "daemon log"),
        (config.routes_path(), "discovered routes"),
        (config.overlay_path(), "overlay state"),
        (config.issues_path(), "visibility issues state"),
        (config.auto_open_path(), "auto-open tracker state"),
        (config.cloud_state_path(), "cloud connection state"),
        (
            config.cloud_route_status_path(),
            "cloud route review status",
        ),
        (config.diagnostics_path(), "diagnostics report"),
        (config.pid_path(), "daemon PID file"),
    ];
    for (path, label) in daemon_files {
        targets.push(PurgeTarget { path, label });
    }

    Ok(targets)
}

/// A file that was removed (or found already absent) by a purge run.
struct RemovedFile {
    path: PathBuf,
    label: &'static str,
}

/// Delete each existing target, returning the ones that were actually present.
///
/// Missing files are not an error: purge is idempotent, so a second run (or a
/// never-used install) simply reports that nothing was left to remove.
fn remove_existing(targets: &[PurgeTarget]) -> Result<Vec<RemovedFile>> {
    let mut removed = Vec::new();
    for target in targets {
        if !target.path.exists() {
            continue;
        }
        std::fs::remove_file(&target.path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to remove {} ({}): {e}\n\n\
                 Check that you own the file and have write permission to \
                 ~/.portzero/, then run `portzero purge` again.",
                target.path.display(),
                target.label,
            )
        })?;
        removed.push(RemovedFile {
            path: target.path.clone(),
            label: target.label,
        });
    }
    Ok(removed)
}

/// Print what was removed (and where) so the user can see exactly what changed.
fn report(removed: &[RemovedFile]) {
    if removed.is_empty() {
        println!("No local Port Zero data found — nothing to remove.");
        println!("Local data lives under ~/.portzero/ (this install had none).");
        return;
    }

    println!("Removed {} local Port Zero file(s):", removed.len());
    for file in removed {
        println!("  - {} ({})", file.path.display(), file.label);
    }
    println!();
    println!(
        "This only erased data on this machine. To delete your account and any \n\
         server-side data, use the dashboard at https://app.portzero.cloud."
    );
}

/// Run `portzero purge`.
///
/// Refuses while the daemon is running so state files are never deleted out
/// from under a live daemon; the user is told exactly how to proceed.
pub fn run() -> Result<()> {
    let config = DaemonConfig::load();

    if let Some(pid) = read_daemon_pid(&config) {
        anyhow::bail!(
            "The Port Zero daemon is still running (PID {pid}).\n\n\
             Purge removes the daemon's own state files, so stop it first:\n\
             \n\
               portzero stop\n\
               portzero purge\n\
             \n\
             Nothing was removed."
        );
    }

    let targets = purge_targets(&config)?;
    let removed = remove_existing(&targets)?;
    report(&removed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `f` with `HOME` pointed at a fresh temp dir, restoring it after.
    /// Shares the process-global HOME lock so it serializes with every other
    /// HOME-mutating test in this binary.
    fn with_temp_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = crate::home_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "pz-purge-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &dir);
        let out = f(&dir);
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    fn touch(path: &std::path::Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn targets_cover_the_documented_personal_data_files() {
        with_temp_home(|_| {
            let config = DaemonConfig::default();
            let targets = purge_targets(&config).unwrap();
            let paths: Vec<String> = targets
                .iter()
                .map(|t| t.path.display().to_string())
                .collect();

            for needle in [
                "auth.json",
                "cost/store.json",
                "observations.json",
                "daemon.log",
                "routes.json",
                "cloud_state.json",
                "diagnostics.json",
            ] {
                assert!(
                    paths.iter().any(|p| p.contains(needle)),
                    "expected a target containing {needle}, got {paths:?}"
                );
            }
        });
    }

    #[test]
    fn remove_existing_deletes_present_files_and_leaves_others_missing() {
        with_temp_home(|_| {
            let config = DaemonConfig::default();

            // Create a representative subset of the personal-data files.
            let auth = AuthConfig::path().unwrap();
            let cost = cost_store::store_path().unwrap();
            let obs = config.observations_path();
            let log = config.log_path();
            for p in [&auth, &cost, &obs, &log] {
                touch(p);
            }
            // Leave routes.json absent to prove missing files are tolerated.
            let routes = config.routes_path();
            assert!(!routes.exists());

            let targets = purge_targets(&config).unwrap();
            let removed = remove_existing(&targets).unwrap();

            // Every file we created is now gone.
            for p in [&auth, &cost, &obs, &log] {
                assert!(!p.exists(), "expected {} to be removed", p.display());
            }
            // Exactly the four created files were reported as removed.
            assert_eq!(removed.len(), 4, "removed: {removed:?}");
        });
    }

    #[test]
    fn remove_existing_is_idempotent_on_a_clean_install() {
        with_temp_home(|_| {
            let config = DaemonConfig::default();
            let targets = purge_targets(&config).unwrap();
            let removed = remove_existing(&targets).unwrap();
            assert!(removed.is_empty(), "nothing should be removed: {removed:?}");
        });
    }
}

impl std::fmt::Debug for RemovedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.path.display(), self.label)
    }
}
