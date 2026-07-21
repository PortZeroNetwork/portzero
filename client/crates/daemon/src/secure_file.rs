//! Symlink-safe atomic file persistence helpers.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Create a private directory and keep it owner-only on Unix.
pub fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("set private directory mode on {}", path.display()))?;
    }
    Ok(())
}

/// Atomically replace `path` with an owner-readable secret file.
pub fn write_secret_atomic(path: &Path, content: &[u8]) -> Result<()> {
    write_atomic(path, content, 0o600)
}

/// Atomically replace `path` with a non-secret, world-readable file.
pub(crate) fn write_public_atomic(path: &Path, content: &[u8]) -> Result<()> {
    write_atomic(path, content, 0o644)
}

fn random_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()))
}

fn write_atomic(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    let tmp = random_sibling(path);

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }

    let result = (|| -> Result<()> {
        let mut file = options
            .open(&tmp)
            .with_context(|| format!("create temporary file {}", tmp.display()))?;
        file.write_all(content)
            .with_context(|| format!("write temporary file {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary file {}", tmp.display()))?;
        drop(file);

        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(path).with_context(|| format!("replace {}", path.display()))?;
        }

        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))?;

        #[cfg(unix)]
        {
            let directory = std::fs::File::open(parent)
                .with_context(|| format!("open directory {} for sync", parent.display()))?;
            directory
                .sync_all()
                .with_context(|| format!("sync directory {}", parent.display()))?;
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_directory_is_owner_only() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_dir(&private).unwrap();

        assert_eq!(std::fs::metadata(&private).unwrap().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn secret_is_owner_only_and_replaces_symlink_without_following_it() {
        use std::os::unix::fs::{symlink, MetadataExt};

        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        let secret = dir.path().join("secret");
        std::fs::write(&victim, b"unchanged").unwrap();
        symlink(&victim, &secret).unwrap();

        write_secret_atomic(&secret, b"new secret").unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
        assert_eq!(std::fs::read(&secret).unwrap(), b"new secret");
        assert!(!std::fs::symlink_metadata(&secret)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::metadata(&secret).unwrap().mode() & 0o777, 0o600);
    }
}
