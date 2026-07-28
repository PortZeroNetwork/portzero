//! Owner scoping for process discovery ([[decision-2]] / task-94).
//!
//! On macOS the daemon runs as a root LaunchDaemon and can read **every**
//! account's process environments; Windows has the same reach under the
//! Administrator service model. Without scoping, another account's process
//! that sets `PZ_TUNNEL` would be tunneled without consent — and a cloud
//! value would publish under the *owner's* cloud account. Port Zero is
//! single-owner-per-machine, so discovery only acts on processes owned by
//! the daemon owner's user.
//!
//! Linux needs no filter for correctness (the unprivileged user service
//! cannot read other accounts' `/proc/<pid>/environ`), but the same scope is
//! applied there so a `sudo`-launched daemon behaves identically and no
//! doomed environment reads are attempted.
//!
//! The owner is resolved once per daemon lifetime:
//! - an unprivileged daemon is owned by its own user;
//! - a root daemon resolves the real user from `SUDO_UID` (manual `sudo`
//!   runs) or the owner of the pinned `HOME` directory (the LaunchDaemon
//!   sets `HOME` to the installing user's home — see `autostart.rs`);
//! - a Windows daemon running as a service account (SYSTEM / LocalService /
//!   NetworkService) has no resolvable owner.
//!
//! When no owner can be resolved, discovery is deliberately left unscoped
//! (previous behavior) with a warning, rather than silently discovering
//! nothing.

use std::sync::OnceLock;

use sysinfo::{Process, System, Uid};

/// Which processes discovery may act on. Construct per scan with
/// [`OwnerScope::detect`]; the underlying owner resolution is cached.
pub(in crate::discovery) struct OwnerScope {
    owner: Option<Uid>,
}

impl OwnerScope {
    pub(in crate::discovery) fn detect(sys: &System) -> Self {
        static OWNER: OnceLock<Option<Uid>> = OnceLock::new();
        Self {
            owner: OWNER.get_or_init(|| detect_uncached(sys)).clone(),
        }
    }

    /// Whether discovery may act on this process. A process whose user is
    /// unknown is treated as foreign while an owner is in effect — it cannot
    /// be attributed to the owner, so it must not be tunneled or published.
    pub(in crate::discovery) fn permits(&self, process: Option<&Process>) -> bool {
        permits_uid(self.owner.as_ref(), process.and_then(|p| p.user_id()))
    }

    /// Emit one debug line per scan so a shared-machine user can see why
    /// their process was not picked up.
    pub(in crate::discovery) fn log_skipped(&self, skipped: usize, scan: &str) {
        if skipped > 0 {
            tracing::debug!(
                skipped,
                scan,
                "skipped processes owned by other user accounts (owner-scoped discovery)"
            );
        }
    }
}

fn permits_uid(owner: Option<&Uid>, process_uid: Option<&Uid>) -> bool {
    match owner {
        None => true,
        Some(owner) => process_uid == Some(owner),
    }
}

#[cfg(unix)]
fn detect_uncached(sys: &System) -> Option<Uid> {
    let Some(self_uid) = daemon_process_uid(sys) else {
        tracing::warn!(
            "could not determine the daemon's own user; process discovery is not owner-scoped"
        );
        return None;
    };
    match resolve_unix_owner(self_uid, sudo_uid_env(), home_owner_uid()) {
        Some(uid) => Uid::try_from(uid as usize).ok(),
        None => {
            tracing::warn!(
                "daemon is running as root with no SUDO_UID and no user-owned HOME, so the \
                 owning user cannot be determined; process discovery is not owner-scoped \
                 (processes of all accounts are eligible)"
            );
            None
        }
    }
}

#[cfg(windows)]
fn detect_uncached(sys: &System) -> Option<Uid> {
    let Some(process) = sys.process(sysinfo::Pid::from_u32(std::process::id())) else {
        tracing::warn!(
            "could not determine the daemon's own user; process discovery is not owner-scoped"
        );
        return None;
    };
    match process.user_id() {
        Some(sid) if !is_windows_service_sid(sid) => Some(sid.clone()),
        _ => {
            tracing::warn!(
                "daemon is running under a Windows service account, so the owning user \
                 cannot be determined; process discovery is not owner-scoped \
                 (processes of all accounts are eligible)"
            );
            None
        }
    }
}

#[cfg(unix)]
fn daemon_process_uid(sys: &System) -> Option<u32> {
    sys.process(sysinfo::Pid::from_u32(std::process::id()))
        .and_then(|p| p.user_id())
        .map(|u| **u)
}

/// Pure owner resolution for Unix: an unprivileged daemon is its own owner; a
/// root daemon belongs to the real user behind `sudo` or the pinned `HOME`.
/// Root is never an owner — a root-only filter would make discovery useless.
#[cfg(unix)]
fn resolve_unix_owner(
    self_uid: u32,
    sudo_uid: Option<u32>,
    home_owner_uid: Option<u32>,
) -> Option<u32> {
    if self_uid != 0 {
        return Some(self_uid);
    }
    sudo_uid
        .filter(|uid| *uid != 0)
        .or_else(|| home_owner_uid.filter(|uid| *uid != 0))
}

#[cfg(unix)]
fn sudo_uid_env() -> Option<u32> {
    std::env::var("SUDO_UID").ok()?.parse().ok()
}

#[cfg(unix)]
fn home_owner_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let home = std::env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    std::fs::metadata(&home).ok().map(|m| m.uid())
}

/// SIDs of the built-in Windows service accounts (SYSTEM, LocalService,
/// NetworkService) — none of them is a real owning user.
#[cfg(windows)]
fn is_windows_service_sid(sid: &Uid) -> bool {
    use std::str::FromStr;
    ["S-1-5-18", "S-1-5-19", "S-1-5-20"]
        .iter()
        .any(|s| Uid::from_str(s).ok().as_ref() == Some(sid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn uid(raw: u32) -> Uid {
        Uid::try_from(raw as usize).expect("uid")
    }

    #[cfg(unix)]
    #[test]
    fn permits_only_the_owner_when_scoped() {
        assert!(permits_uid(Some(&uid(501)), Some(&uid(501))));
        assert!(!permits_uid(Some(&uid(501)), Some(&uid(502))));
        assert!(!permits_uid(Some(&uid(501)), Some(&uid(0))));
        // Unknown process user cannot be attributed to the owner.
        assert!(!permits_uid(Some(&uid(501)), None));
    }

    #[cfg(unix)]
    #[test]
    fn permits_everything_when_unscoped() {
        assert!(permits_uid(None, Some(&uid(502))));
        assert!(permits_uid(None, None));
    }

    #[cfg(unix)]
    #[test]
    fn unprivileged_daemon_owns_itself() {
        assert_eq!(resolve_unix_owner(501, None, None), Some(501));
        // SUDO_UID / HOME never override a non-root daemon's own identity.
        assert_eq!(resolve_unix_owner(501, Some(777), Some(888)), Some(501));
    }

    #[cfg(unix)]
    #[test]
    fn root_daemon_resolves_sudo_uid_then_home_owner() {
        assert_eq!(resolve_unix_owner(0, Some(501), Some(502)), Some(501));
        assert_eq!(resolve_unix_owner(0, None, Some(502)), Some(502));
        assert_eq!(resolve_unix_owner(0, None, None), None);
        // Root is never an owner: a root-owned HOME (e.g. /var/root when the
        // plist did not pin HOME) must not scope discovery to root processes.
        assert_eq!(resolve_unix_owner(0, Some(0), Some(0)), None);
    }

    #[cfg(windows)]
    #[test]
    fn service_sids_are_not_owners() {
        use std::str::FromStr;
        for sid in ["S-1-5-18", "S-1-5-19", "S-1-5-20"] {
            assert!(is_windows_service_sid(&Uid::from_str(sid).expect("sid")));
        }
        let user = Uid::from_str("S-1-5-21-3623811015-3361044348-30300820-1013").expect("user sid");
        assert!(!is_windows_service_sid(&user));
    }
}
