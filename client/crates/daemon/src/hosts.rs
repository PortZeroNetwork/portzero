//! Detects whether writing to a hosts file (`/etc/hosts` on Unix,
//! `C:\Windows\System32\drivers\etc\hosts` on Windows) is likely to succeed
//! *and persist*, before `setup`/`doctor` attempt or suggest an edit.
//!
//! A plain permission check isn't enough: a write can "succeed" and still be
//! pointless (a container runtime bind-mounts a fresh file over it on every
//! restart) or a write can fail in a way `io::Error` doesn't explain well
//! (Linux's immutable file attribute rejects writes with a bare
//! `EPERM`/`EACCES`, indistinguishable from a permissions problem unless you
//! check the attribute directly).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostsWriteBlocker {
    /// Linux `chattr +i`: the immutable attribute rejects all writes
    /// regardless of permission bits, including root's (unless
    /// `CAP_LINUX_IMMUTABLE` is held — commonly dropped in containers).
    Immutable,
    /// Not writable for some other reason (permission bits, read-only mount,
    /// missing parent directory, ...). Carries the raw OS error text.
    NotWritable(String),
    /// The path is a symlink to somewhere outside its own directory. Some
    /// distros (e.g. NixOS) symlink the hosts file to a generated
    /// Nix-store path rebuilt from a declarative config; a direct edit is
    /// either rejected (read-only store) or silently discarded on the next
    /// rebuild.
    ManagedSymlink(PathBuf),
    /// The hosts file is its own bind mount (the standard Docker/Kubernetes
    /// shape: the container runtime writes a fresh file over this path on
    /// every container start). The edit will "succeed" but not survive a
    /// restart.
    EphemeralBindMount(String),
}

impl HostsWriteBlocker {
    /// A short, human-facing explanation plus the concrete next step.
    pub fn describe(&self) -> (&'static str, String) {
        match self {
            HostsWriteBlocker::Immutable => (
                "immutable",
                "the file has the immutable attribute set: sudo chattr -i <path>".to_string(),
            ),
            HostsWriteBlocker::NotWritable(err) => (
                "not writable",
                format!("could not open the file for writing: {err}"),
            ),
            HostsWriteBlocker::ManagedSymlink(target) => (
                "managed symlink",
                format!(
                    "the file is a symlink to {} outside its own directory; it is likely \
                     generated (e.g. NixOS's networking.extraHosts) and a direct edit may be \
                     rejected or overwritten on the next rebuild",
                    target.display()
                ),
            ),
            HostsWriteBlocker::EphemeralBindMount(source) => (
                "ephemeral bind mount",
                format!(
                    "the file is bind-mounted from {source}, the standard container-runtime \
                     shape; an edit will not survive a container restart"
                ),
            ),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostsWriteSafety {
    pub blockers: Vec<HostsWriteBlocker>,
}

impl HostsWriteSafety {
    pub fn is_safe(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Check whether `path` is safe to edit in place. Read-only inspection: never
/// modifies the file, aside from a zero-byte open-for-append probe used to
/// distinguish "not writable" from "writable" (this can update the file's
/// access time, nothing else).
pub fn check_hosts_write_safety(path: &Path) -> HostsWriteSafety {
    let mut blockers = Vec::new();

    if let Some(target) = managed_symlink_target(path) {
        blockers.push(HostsWriteBlocker::ManagedSymlink(target));
    }

    if let Some(source) = bind_mounted_over(path) {
        blockers.push(HostsWriteBlocker::EphemeralBindMount(source));
    }

    #[cfg(target_os = "linux")]
    if is_immutable(path) {
        blockers.push(HostsWriteBlocker::Immutable);
    }

    // Only probe raw writability if nothing more specific already explains a
    // failure — immutability and read-only bind mounts both surface as a
    // generic permission error otherwise, which is far less actionable.
    if blockers.is_empty() {
        if let Err(e) = probe_writable(path) {
            blockers.push(HostsWriteBlocker::NotWritable(e.to_string()));
        }
    }

    HostsWriteSafety { blockers }
}

/// Symlinked *within* the same directory (an atomic-rename pattern some tools
/// use for their own writes) isn't a sign of external management. Only a
/// target outside the file's directory is flagged — that's the
/// generated-elsewhere shape (NixOS's `/etc/hosts -> /etc/static/hosts` or
/// similar).
fn managed_symlink_target(path: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(path).ok()?;
    let parent = path.parent().unwrap_or_else(|| Path::new("/"));
    let resolved = if target.is_relative() {
        parent.join(&target)
    } else {
        target.clone()
    };
    let resolved_parent = resolved.parent().unwrap_or_else(|| Path::new("/"));
    if resolved_parent == parent {
        None
    } else {
        Some(target)
    }
}

/// The precise Docker/Kubernetes signature: the container runtime bind-mounts
/// a generated file *directly onto this path*, distinct from (and much more
/// reliable than) generic "am I in a container" heuristics like
/// `/.dockerenv` or `systemd-detect-virt`, which can be true in a VM that
/// happens to report as a container without this path being managed at all.
fn bind_mounted_over(path: &Path) -> Option<String> {
    let mounts = std::fs::read_to_string("/proc/mounts").ok()?;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let device = fields.next()?;
        let mount_point = fields.next()?;
        let fstype = fields.next().unwrap_or("?");
        if Path::new(mount_point) == path {
            return Some(format!("{device} ({fstype})"));
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn is_immutable(path: &Path) -> bool {
    use std::os::unix::io::AsRawFd;

    const FS_IMMUTABLE_FL: libc::c_long = 0x00000010;

    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut flags: libc::c_long = 0;
    let ret = unsafe { libc::ioctl(file.as_raw_fd(), libc::FS_IOC_GETFLAGS, &mut flags) };
    ret == 0 && (flags & FS_IMMUTABLE_FL) != 0
}

fn probe_writable(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new().append(true).open(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_writable_file_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1 localhost\n").unwrap();
        assert!(check_hosts_write_safety(&path).is_safe());
    }

    #[test]
    fn missing_file_is_not_writable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist").join("hosts");
        let safety = check_hosts_write_safety(&path);
        assert!(!safety.is_safe());
        assert!(matches!(
            safety.blockers[0],
            HostsWriteBlocker::NotWritable(_)
        ));
    }

    #[test]
    fn symlink_to_sibling_file_is_safe() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("hosts.real");
        std::fs::write(&real, "127.0.0.1 localhost\n").unwrap();
        let link = dir.path().join("hosts");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(managed_symlink_target(&link).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_outside_directory_is_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let real = elsewhere.path().join("static-hosts");
        std::fs::write(&real, "127.0.0.1 localhost\n").unwrap();
        let link = dir.path().join("hosts");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let safety = check_hosts_write_safety(&link);
        assert!(!safety.is_safe());
        assert!(matches!(
            safety.blockers[0],
            HostsWriteBlocker::ManagedSymlink(_)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn immutable_attribute_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1 localhost\n").unwrap();

        let set = std::process::Command::new("chattr")
            .arg("+i")
            .arg(&path)
            .status();
        let Ok(status) = set else {
            eprintln!("skipping: chattr not available");
            return;
        };
        if !status.success() {
            eprintln!("skipping: chattr +i failed (likely missing CAP_LINUX_IMMUTABLE)");
            return;
        }

        let safety = check_hosts_write_safety(&path);

        // Always undo the attribute before asserting, so a failed assertion
        // doesn't leave an immutable file behind in the temp dir.
        let _ = std::process::Command::new("chattr")
            .arg("-i")
            .arg(&path)
            .status();

        assert!(!safety.is_safe());
        assert!(safety.blockers.contains(&HostsWriteBlocker::Immutable));
    }

    #[test]
    fn no_bind_mount_for_ordinary_tempfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts");
        std::fs::write(&path, "127.0.0.1 localhost\n").unwrap();
        assert_eq!(bind_mounted_over(&path), None);
    }
}
