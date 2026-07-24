//! `portzero update` — download the latest release and replace the running
//! binary in place.
//!
//! This is the applier half of the update feature; [`crate::update`] is the
//! background notifier that tells the user a newer version exists. Both resolve
//! the release location through `portzero_domain::endpoints::update_download_base`,
//! so a repo/asset move or a `version.json` schema change breaks — and is caught
//! by tests on — a single seam.
//!
//! Flow: read `version.json` → compare to the compiled-in version → download the
//! platform archive → unpack it with the OS's own `tar` / `Expand-Archive` (no
//! extra crates, same tools the installers use) → atomically swap the running
//! binary → refresh privileged OS integration via `setup`.
//!
//! The download base is overridable (`PZ_TUNNEL_UPDATE_BASE_URL`) and may be a
//! local directory or `file://` path, which is what lets the vmkit `autoupdate`
//! flavor drive the whole path deterministically and offline.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::update::{parse_semver, VersionManifest};

/// Generous ceiling for the archive download; a real release is a few MB.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// Entry point for the `portzero update` command.
///
/// `check_only` reports availability without changing anything; `force`
/// reinstalls even when the installed version already matches the latest
/// (useful for repairing a broken install).
pub async fn run(check_only: bool, force: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let base = portzero_domain::endpoints::update_download_base();

    let latest = match fetch_manifest_version(&base).await? {
        Some(v) => v,
        None => bail!(
            "Could not read the release manifest (version.json) from {base}.\n\
             Next steps: check your network connection and try again, or download \
             the latest release manually from {}.",
            portzero_domain::endpoints::releases_url()
        ),
    };

    let newer = is_newer(&latest, current);

    if check_only {
        report_check(current, &latest, newer);
        return Ok(());
    }

    if !newer && !force {
        println!("portzero is already up to date (v{current}).");
        return Ok(());
    }

    apply_update(&base, current, &latest).await
}

/// Fetch and parse `version.json`, returning its `version` field.
///
/// `Ok(None)` means the manifest was not found (missing asset / 404); a parse
/// failure is a hard error because it means the manifest shape drifted from what
/// every installed client expects.
async fn fetch_manifest_version(base: &str) -> Result<Option<String>> {
    let Some(bytes) = fetch_bytes(base, "version.json").await? else {
        return Ok(None);
    };
    let manifest: VersionManifest = serde_json::from_slice(&bytes).context(
        "release manifest (version.json) was not the expected {\"version\":\"…\"} shape",
    )?;
    Ok(Some(manifest.version))
}

/// True when `latest` is a strictly greater SemVer than `current`. Anything that
/// fails to parse is treated as "not newer" so a malformed tag never triggers an
/// update — the same conservative rule the notifier uses.
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn report_check(current: &str, latest: &str, newer: bool) {
    if newer {
        println!("An update is available: v{current} -> v{latest}");
        println!("Run `portzero update` to install it.");
    } else {
        println!("portzero is up to date (installed v{current}, latest v{latest}).");
    }
}

/// Download, unpack, and swap in the latest release.
async fn apply_update(base: &str, current: &str, latest: &str) -> Result<()> {
    let target = target_triple()?;
    let archive_rel = archive_name(&target);

    println!("Downloading portzero v{latest} ({target})…");
    let Some(bytes) = fetch_bytes(base, &archive_rel).await? else {
        bail!(
            "Release archive `{archive_rel}` was not found at {base}.\n\
             Next steps: confirm a v{latest} release published a {target} build, \
             or install manually from {}.",
            portzero_domain::endpoints::releases_url()
        );
    };

    let workdir = make_workdir()?;
    let archive_path = workdir.join(&archive_rel);
    std::fs::write(&archive_path, &bytes).with_context(|| {
        format!(
            "writing the downloaded archive to {}",
            archive_path.display()
        )
    })?;

    extract(&archive_path, &workdir)?;
    let new_bin = find_binary(&workdir)?;

    let installed = replace_running_binary(&new_bin).context(
        "Could not replace the current binary. If it lives in a system directory, \
         re-run with elevated privileges (e.g. `sudo portzero update`).",
    )?;
    // Best effort: the extracted files are large; leaving them wastes temp space.
    let _ = std::fs::remove_dir_all(&workdir);

    println!(
        "Updated portzero: v{current} -> v{latest} ({})",
        installed.display()
    );
    refresh_system_integration(&installed);
    Ok(())
}

/// `<os>-<arch>` release identifier, matching the archive names built by
/// `.github/workflows/release.yml` (e.g. `linux-amd64`, `darwin-arm64`,
/// `windows-amd64`).
fn target_triple() -> Result<String> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        bail!(
            "self-update is not supported on this operating system; install manually from {}.",
            portzero_domain::endpoints::releases_url()
        );
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => bail!(
            "self-update has no prebuilt binary for CPU architecture `{other}`; \
             install manually from {}.",
            portzero_domain::endpoints::releases_url()
        ),
    };
    Ok(format!("{os}-{arch}"))
}

fn archive_name(target: &str) -> String {
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    format!("portzero-{target}.{ext}")
}

fn binary_file_name() -> &'static str {
    if cfg!(windows) {
        "portzero.exe"
    } else {
        "portzero"
    }
}

/// Fetch `rel` under `base`. `base` is either an HTTP(S) URL or a local
/// directory (an absolute path or a `file://` URL), the latter being how the
/// test harness serves a throwaway release mirror. `Ok(None)` == not found.
async fn fetch_bytes(base: &str, rel: &str) -> Result<Option<Vec<u8>>> {
    if let Some(dir) = local_base_dir(base) {
        let path = dir.join(rel);
        return match std::fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
        };
    }

    let url = format!("{}/{}", base.trim_end_matches('/'), rel);
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        bail!("GET {url} returned HTTP {}", resp.status());
    }
    Ok(Some(resp.bytes().await?.to_vec()))
}

/// Interpret `base` as a local directory when it is a `file://` URL or an
/// existing absolute directory path; otherwise it is a remote URL.
fn local_base_dir(base: &str) -> Option<PathBuf> {
    if let Some(rest) = base.strip_prefix("file://") {
        return Some(PathBuf::from(rest));
    }
    let path = Path::new(base);
    if path.is_absolute() && path.is_dir() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

fn make_workdir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("portzero-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating work directory {}", dir.display()))?;
    Ok(dir)
}

/// Unpack `archive` into `out` using the OS's own archiver — `tar` on Unix,
/// `Expand-Archive` (PowerShell) on Windows — the same tools the installers use.
fn extract(archive: &Path, out: &Path) -> Result<()> {
    #[cfg(windows)]
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            archive.display(),
            out.display()
        ))
        .status()
        .context("running Expand-Archive (PowerShell) to unpack the update")?;

    #[cfg(not(windows))]
    let status = Command::new("tar")
        .arg("xzf")
        .arg(archive)
        .arg("-C")
        .arg(out)
        .status()
        .context("running `tar` to unpack the update (is `tar` on PATH?)")?;

    if !status.success() {
        bail!("unpacking {} failed ({status})", archive.display());
    }
    Ok(())
}

/// Depth-first search for the `portzero[.exe]` file inside the unpacked archive
/// (release archives nest it one directory down, e.g. `portzero-linux-amd64/`).
fn find_binary(root: &Path) -> Result<PathBuf> {
    let want = binary_file_name();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else if entry.file_name() == want {
                return Ok(entry.path());
            }
        }
    }
    bail!("the downloaded update did not contain a `{want}` binary")
}

/// Swap the new binary in for the currently running one, returning the path that
/// was replaced. On Unix this is an atomic same-directory rename (the running
/// process keeps executing the old, now-unlinked inode). On Windows the running
/// `.exe` can't be overwritten, so it is renamed aside first.
#[cfg(unix)]
fn replace_running_binary(new_bin: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let current = std::env::current_exe().context("locating the current executable")?;
    let dir = current.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "current binary {} has no parent directory",
            current.display()
        )
    })?;

    let staged = dir.join(format!(".portzero-update-{}", std::process::id()));
    std::fs::copy(new_bin, &staged)
        .with_context(|| format!("staging the new binary in {}", dir.display()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("setting exec bit on {}", staged.display()))?;
    std::fs::rename(&staged, &current).with_context(|| {
        let _ = std::fs::remove_file(&staged);
        format!("replacing {}", current.display())
    })?;
    Ok(current)
}

#[cfg(windows)]
fn replace_running_binary(new_bin: &Path) -> Result<PathBuf> {
    let current = std::env::current_exe().context("locating the current executable")?;
    let backup = current.with_extension("old");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&current, &backup)
        .with_context(|| format!("moving the running binary {} aside", current.display()))?;
    if let Err(e) = std::fs::copy(new_bin, &current) {
        // Roll back so the user is never left with no binary at all.
        let _ = std::fs::rename(&backup, &current);
        return Err(anyhow::Error::new(e).context(format!(
            "installing the new binary at {}",
            current.display()
        )));
    }
    // The moved-aside file is locked while this process runs; cleaned next launch.
    let _ = std::fs::remove_file(&backup);
    Ok(current)
}

/// Re-run the privileged `setup` so OS integration (autostart service, CA trust,
/// DNS resolver, hosts pin) matches the new binary. Only auto-run when we
/// already hold the needed privilege; otherwise print the one command to run,
/// so `update` never blocks on an interactive sudo prompt.
fn refresh_system_integration(installed: &Path) {
    // Escape hatch for tests (and users who only want the binary swapped): skip
    // the privileged refresh entirely. The vmkit `autoupdate` flavor uses this to
    // exercise the download → extract → swap path hermetically; the combined
    // flavor already covers the full privileged install/setup path.
    if std::env::var_os("PZ_TUNNEL_UPDATE_SKIP_SETUP").is_some() {
        println!("Skipping system-integration refresh (PZ_TUNNEL_UPDATE_SKIP_SETUP set).");
        return;
    }
    if !privileged() {
        println!(
            "The binary is updated. Run `sudo portzero setup` to refresh system \
             integration (autostart service, local CA, DNS resolver) for the new version."
        );
        return;
    }
    println!("Refreshing system integration…");
    match Command::new(installed).arg("setup").status() {
        Ok(s) if s.success() => println!("System integration refreshed."),
        Ok(s) => println!("`portzero setup` exited with {s}; re-run it if tunnels misbehave."),
        Err(e) => println!("Could not run `portzero setup` ({e}); re-run it manually."),
    }
}

#[cfg(unix)]
fn privileged() -> bool {
    // geteuid never fails and has no safety preconditions.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(windows)]
fn privileged() -> bool {
    // The MSI / scheduled-task update context is already elevated; let `setup`
    // surface its own permission error in the rare case it is not.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_detects_upgrade() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.1", "0.1.0"));
    }

    #[test]
    fn is_newer_rejects_same_or_older() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn is_newer_treats_unparseable_as_not_newer() {
        // A malformed latest tag must never trigger a self-update.
        assert!(!is_newer("not-a-version", "0.1.0"));
        assert!(!is_newer("0.1.0", "garbage"));
        assert!(!is_newer("1.2.3.4", "0.1.0"));
    }

    #[test]
    fn archive_name_matches_release_convention() {
        let name = archive_name("linux-amd64");
        if cfg!(windows) {
            assert_eq!(name, "portzero-linux-amd64.zip");
        } else {
            assert_eq!(name, "portzero-linux-amd64.tar.gz");
        }
    }

    #[test]
    fn local_base_dir_recognises_file_url() {
        assert_eq!(
            local_base_dir("file:///tmp/mirror"),
            Some(PathBuf::from("/tmp/mirror"))
        );
    }

    #[test]
    fn local_base_dir_recognises_existing_dir() {
        let tmp = std::env::temp_dir();
        assert_eq!(local_base_dir(tmp.to_str().unwrap()), Some(tmp));
    }

    #[test]
    fn local_base_dir_treats_http_as_remote() {
        assert_eq!(local_base_dir("https://example.com/releases"), None);
        assert_eq!(local_base_dir("http://127.0.0.1:8080/x"), None);
    }

    #[test]
    fn find_binary_locates_nested_binary() {
        let root = std::env::temp_dir().join(format!("pz-find-{}", std::process::id()));
        let nested = root.join("portzero-linux-amd64");
        std::fs::create_dir_all(&nested).unwrap();
        let bin = nested.join(binary_file_name());
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        // A decoy sibling that is not the binary must be skipped.
        std::fs::write(nested.join("README"), b"x").unwrap();

        let found = find_binary(&root).unwrap();
        assert_eq!(found, bin);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_binary_errors_when_absent() {
        let root = std::env::temp_dir().join(format!("pz-empty-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(find_binary(&root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
