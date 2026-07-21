//! Linux trust-store installation.
//!
//! Two independent stores are provisioned:
//!
//! 1. **System CA bundle** — adds the cert to the distro CA store so OpenSSL
//!    and system tools trust it immediately. Writing it needs root, which the
//!    daemon does NOT have when running as an unprivileged systemd *user*
//!    service; in that case this step is skipped with an actionable warning
//!    (`sudo portzero trust install`) and the NSS step below still runs.
//!    - Debian/Ubuntu: copy to `/usr/share/ca-certificates/portzero/`, register
//!      in `/etc/ca-certificates.conf`, then run `update-ca-certificates`.
//!    - RHEL/Fedora:   copy to `/etc/pki/ca-trust/source/anchors/` + `update-ca-trust extract`
//!
//! 2. **NSS databases** (Chrome, Chromium, Firefox) — browsers maintain their
//!    own certificate stores independent of the system CA bundle. These live in
//!    the user's home and are writable without root, so the user service can
//!    provision browser trust on its own. Added via `certutil` (from the
//!    `libnss3-tools` / `nss-tools` package). Best-effort: if `certutil` is
//!    absent a clear, actionable warning is logged.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::common::{
    command_exists, debian_ssl_cert_path, delete_nss_cert, find_nss_dbs, install_nss_cert,
    legacy_system_cert_dest_debian, native_chromium_nss_db, nss_db_contains_ca,
    p11_kit_anchor_path, run_command, system_cert_dest_debian, system_cert_dest_rhel,
};
use super::TrustVerificationReport;

const DEBIAN_CA_CONFIG_LINE: &str = "portzero/portzero-local-ca.crt";

/// Which distro-level CA update tool is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinuxCaTool {
    /// Debian / Ubuntu / Mint: `update-ca-certificates`.
    UpdateCaCertificates,
    /// RHEL / Fedora / CentOS: `update-ca-trust`.
    UpdateCaTrust,
    /// Nothing found — log a warning.
    None,
}

/// Snapshot of the host trust environment, captured so detection is a pure
/// function testable without touching the real system.
#[derive(Debug)]
pub(crate) struct LinuxTrustEnv {
    pub(crate) ca_tool: LinuxCaTool,
    /// Absolute path to `certutil`, if available.
    pub(crate) certutil_path: Option<PathBuf>,
    /// All NSS database directories found in the user's home.
    pub(crate) nss_db_dirs: Vec<PathBuf>,
    /// NSS database directories that should be created if missing.
    ///
    /// Native Chromium-family browsers on Linux, including apt-installed Brave,
    /// use `~/.pki/nssdb`. Seeding it before the browser exists makes browsers
    /// installed after PortZero trust the local CA on first launch.
    pub(crate) provision_nss_db_dirs: Vec<PathBuf>,
}

pub(super) fn install_impl(ca_cert_path: &Path) -> Result<()> {
    let env = detect_trust_env();
    // Writing the system CA bundle needs root, which the daemon does NOT have
    // when it runs as an unprivileged systemd *user* service (it carries only
    // CAP_NET_ADMIN + CAP_NET_BIND_SERVICE — see autostart.rs). Keep going even
    // if that step is skipped so the per-user NSS databases, which ARE writable
    // without root, still get the CA and browsers trust *.portzero.local.
    let system_result = install_system_ca(ca_cert_path, &env);
    install_nss_dbs(ca_cert_path, &env);
    system_result
}

pub(super) fn uninstall_impl() -> Result<()> {
    let env = detect_trust_env();
    uninstall_system_ca(&env)?;
    uninstall_nss_dbs(&env);
    Ok(())
}

pub(super) fn verify_installation_impl(ca_cert_path: &Path) -> Result<TrustVerificationReport> {
    let env = detect_trust_env();
    let mut missing = Vec::new();

    match env.ca_tool {
        LinuxCaTool::UpdateCaCertificates => {
            verify_file_matches(
                ca_cert_path,
                &system_cert_dest_debian(),
                "Linux system CA bundle",
                &mut missing,
            );
            verify_file_matches(
                ca_cert_path,
                &debian_ssl_cert_path(),
                "Linux OpenSSL CA bundle",
                &mut missing,
            );
            verify_file_matches(
                ca_cert_path,
                &p11_kit_anchor_path(),
                "Linux p11-kit anchor",
                &mut missing,
            );
            verify_file_contains(
                "portzero/portzero-local-ca.crt",
                &debian_ca_config_path(),
                "Linux CA certificates config",
                &mut missing,
            );
        }
        LinuxCaTool::UpdateCaTrust => {
            verify_file_matches(
                ca_cert_path,
                &system_cert_dest_rhel(),
                "Linux system CA bundle",
                &mut missing,
            );
        }
        LinuxCaTool::None => {
            missing.push(
                "Linux system CA update tool not found (update-ca-certificates or update-ca-trust)"
                    .to_string(),
            );
        }
    }

    if env.nss_db_dirs.is_empty() {
        tracing::debug!("verify_installation: no NSS databases found to check");
    } else if let Some(certutil) = env.certutil_path.as_deref() {
        for db_dir in &env.nss_db_dirs {
            if !nss_db_contains_ca(certutil, db_dir) {
                missing.push(format!("NSS database {}", db_dir.display()));
            }
        }
    } else {
        missing.push(format!(
            "{} NSS database(s) were found but certutil is not available",
            env.nss_db_dirs.len()
        ));
    }

    Ok(TrustVerificationReport { missing })
}

fn install_system_ca(ca_cert_path: &Path, env: &LinuxTrustEnv) -> Result<()> {
    match env.ca_tool {
        LinuxCaTool::UpdateCaCertificates => {
            let dest = system_cert_dest_debian();
            if !copy_system_anchor(ca_cert_path, &dest)? {
                return Ok(());
            }
            ensure_debian_ca_config_line()?;
            remove_legacy_debian_anchor()?;
            run_command("update-ca-certificates", &[] as &[&str])
                .context("update-ca-certificates")?;
            materialize_debian_ssl_cert(ca_cert_path)?;
            install_p11_kit_anchor_file(ca_cert_path)?;
            tracing::info!(
                "Linux: installed CA cert to {} via update-ca-certificates",
                dest.display()
            );
        }
        LinuxCaTool::UpdateCaTrust => {
            let dest = system_cert_dest_rhel();
            if !copy_system_anchor(ca_cert_path, &dest)? {
                return Ok(());
            }
            run_command("update-ca-trust", &["extract"] as &[&str])
                .context("update-ca-trust extract")?;
            tracing::info!(
                "Linux: installed CA cert to {} via update-ca-trust",
                dest.display()
            );
        }
        LinuxCaTool::None => {
            tracing::warn!(
                "Linux: no system CA update tool found (tried update-ca-certificates, \
                 update-ca-trust). To trust *.portzero.local in system tools, copy {} \
                 to your distro's CA anchor directory and run the appropriate update command.",
                ca_cert_path.display()
            );
        }
    }
    Ok(())
}

/// Copy the CA PEM into a root-owned system anchor directory.
///
/// Returns `Ok(true)` if the copy succeeded, `Ok(false)` if it was skipped only
/// because we lack root — the expected case for the unprivileged user service,
/// where the per-user NSS store still gives browsers trust and system-wide trust
/// (curl, openssl) needs `sudo portzero trust install` — or `Err` for any other
/// failure.
fn copy_system_anchor(ca_cert_path: &Path, dest: &Path) -> Result<bool> {
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                tracing::warn!(
                    "Linux: no permission to create system CA directory at {} \
                     (the daemon is running unprivileged). Browsers using the per-user \
                     NSS store will still trust *.portzero.local; for system-wide trust \
                     (curl, openssl, Brave Snap) run: sudo portzero trust install",
                    parent.display()
                );
                return Ok(false);
            }
            return Err(e).with_context(|| format!("create {}", parent.display()));
        }
    }
    match std::fs::copy(ca_cert_path, dest) {
        Ok(_) => {
            set_world_readable(dest)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            tracing::warn!(
                "Linux: no permission to write the system CA store at {} \
                 (the daemon is running unprivileged). Browsers using the per-user \
                 NSS store will still trust *.portzero.local; for system-wide trust \
                 (curl, openssl, Brave Snap) run: sudo portzero trust install",
                dest.display()
            );
            Ok(false)
        }
        Err(e) => Err(e).with_context(|| format!("copy CA cert to {}", dest.display())),
    }
}

fn uninstall_system_ca(env: &LinuxTrustEnv) -> Result<()> {
    match env.ca_tool {
        LinuxCaTool::UpdateCaCertificates => {
            let dest = system_cert_dest_debian();
            let legacy_dest = legacy_system_cert_dest_debian();
            remove_p11_kit_anchor_file()?;
            if dest.exists() {
                std::fs::remove_file(&dest)
                    .with_context(|| format!("remove {}", dest.display()))?;
            }
            if legacy_dest.exists() {
                std::fs::remove_file(&legacy_dest)
                    .with_context(|| format!("remove {}", legacy_dest.display()))?;
            }
            let ssl_cert = debian_ssl_cert_path();
            if ssl_cert.exists() {
                std::fs::remove_file(&ssl_cert)
                    .with_context(|| format!("remove {}", ssl_cert.display()))?;
            }
            remove_debian_ca_config_line()?;
            run_command("update-ca-certificates", &["--fresh"] as &[&str])
                .context("update-ca-certificates --fresh")?;
            tracing::info!(
                "Linux: removed CA cert {} via update-ca-certificates",
                dest.display()
            );
        }
        LinuxCaTool::UpdateCaTrust => {
            let dest = system_cert_dest_rhel();
            if dest.exists() {
                std::fs::remove_file(&dest)
                    .with_context(|| format!("remove {}", dest.display()))?;
                run_command("update-ca-trust", &["extract"] as &[&str])
                    .context("update-ca-trust extract")?;
                tracing::info!(
                    "Linux: removed CA cert {} via update-ca-trust",
                    dest.display()
                );
            }
        }
        LinuxCaTool::None => {}
    }
    Ok(())
}

fn debian_ca_config_path() -> PathBuf {
    PathBuf::from("/etc/ca-certificates.conf")
}

fn verify_file_matches(reference: &Path, target: &Path, label: &str, missing: &mut Vec<String>) {
    match (std::fs::read(reference), std::fs::read(target)) {
        (Ok(expected), Ok(actual)) if expected == actual => {}
        _ => missing.push(format!("{label} at {}", target.display())),
    }
}

fn verify_file_contains(expected: &str, path: &Path, label: &str, missing: &mut Vec<String>) {
    match std::fs::read_to_string(path) {
        Ok(content) if content.lines().any(|line| line.trim() == expected) => {}
        _ => missing.push(format!("{label} at {}", path.display())),
    }
}

fn install_nss_dbs(ca_cert_path: &Path, env: &LinuxTrustEnv) {
    let Some(ref certutil) = env.certutil_path else {
        if !env.nss_db_dirs.is_empty() || !env.provision_nss_db_dirs.is_empty() {
            tracing::warn!(
                "Linux: NSS databases found or expected ({} location(s)) but certutil is not installed. \
                 Browsers may not trust *.portzero.local. \
                 Fix: sudo apt install libnss3-tools  (or dnf install nss-tools)",
                env.nss_db_dirs.len() + env.provision_nss_db_dirs.len()
            );
        }
        return;
    };

    let mut db_dirs = env.nss_db_dirs.clone();
    for db_dir in &env.provision_nss_db_dirs {
        if !db_dirs.contains(db_dir) {
            db_dirs.push(db_dir.clone());
        }
    }

    for db_dir in &db_dirs {
        if let Err(e) = install_nss_cert(certutil, db_dir, ca_cert_path) {
            tracing::warn!(
                "Linux: failed to add CA to NSS DB {}: {e:#}",
                db_dir.display()
            );
        } else {
            tracing::info!("Linux: added CA to NSS DB {}", db_dir.display());
        }
    }
}

fn uninstall_nss_dbs(env: &LinuxTrustEnv) {
    let Some(ref certutil) = env.certutil_path else {
        return;
    };
    for db_dir in &env.nss_db_dirs {
        if let Err(e) = delete_nss_cert(certutil, db_dir) {
            tracing::warn!(
                "Linux: failed to remove CA from NSS DB {}: {e:#}",
                db_dir.display()
            );
        } else {
            tracing::info!("Linux: removed CA from NSS DB {}", db_dir.display());
        }
    }
}

fn ensure_debian_ca_config_line() -> Result<()> {
    update_debian_ca_config(true)
}

fn remove_debian_ca_config_line() -> Result<()> {
    update_debian_ca_config(false)
}

fn update_debian_ca_config(install: bool) -> Result<()> {
    let path = Path::new("/etc/ca-certificates.conf");
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<&str> = content
        .lines()
        .filter(|line| line.trim() != DEBIAN_CA_CONFIG_LINE)
        .collect();
    if install {
        lines.push(DEBIAN_CA_CONFIG_LINE);
    }

    let mut updated = lines.join("\n");
    updated.push('\n');
    std::fs::write(path, updated).with_context(|| format!("write {}", path.display()))
}

fn remove_legacy_debian_anchor() -> Result<()> {
    let legacy_dest = legacy_system_cert_dest_debian();
    if legacy_dest.exists() {
        std::fs::remove_file(&legacy_dest)
            .with_context(|| format!("remove legacy {}", legacy_dest.display()))?;
    }
    Ok(())
}

fn materialize_debian_ssl_cert(ca_cert_path: &Path) -> Result<()> {
    let ssl_cert = debian_ssl_cert_path();
    if ssl_cert.exists() {
        std::fs::remove_file(&ssl_cert)
            .with_context(|| format!("remove {}", ssl_cert.display()))?;
    }
    std::fs::copy(ca_cert_path, &ssl_cert)
        .with_context(|| format!("copy CA cert to {}", ssl_cert.display()))?;
    set_world_readable(&ssl_cert)?;
    Ok(())
}

fn install_p11_kit_anchor_file(ca_cert_path: &Path) -> Result<()> {
    let dest = p11_kit_anchor_path();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::copy(ca_cert_path, &dest)
        .with_context(|| format!("copy CA cert to {}", dest.display()))?;
    set_world_readable(&dest)?;
    refresh_p11_kit_compat();
    Ok(())
}

fn remove_p11_kit_anchor_file() -> Result<()> {
    let dest = p11_kit_anchor_path();
    if dest.exists() {
        std::fs::remove_file(&dest).with_context(|| format!("remove {}", dest.display()))?;
        refresh_p11_kit_compat();
    }
    Ok(())
}

fn refresh_p11_kit_compat() {
    if command_exists("trust") {
        if let Err(e) = run_command("trust", &["extract-compat"] as &[&str]) {
            tracing::warn!("Linux: failed to refresh p11-kit compatibility bundles: {e:#}");
        }
    } else {
        tracing::warn!(
            "Linux: p11-kit trust tool not found; Snap Chromium/Brave may not pick up \
             the local CA until p11-kit compatibility bundles are refreshed."
        );
    }
}

fn set_world_readable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
        .with_context(|| format!("set permissions on {}", path.display()))
}

fn detect_trust_env() -> LinuxTrustEnv {
    let home = effective_home_dir();
    LinuxTrustEnv {
        ca_tool: detect_ca_tool(),
        certutil_path: find_certutil(),
        nss_db_dirs: home.as_deref().map(find_nss_dbs).unwrap_or_default(),
        provision_nss_db_dirs: home
            .as_deref()
            .map(|h| vec![native_chromium_nss_db(h)])
            .unwrap_or_default(),
    }
}

/// Resolve the real user's home directory.
///
/// When running under `sudo`, `$SUDO_USER` names the original caller. We
/// look up their home from `/etc/passwd` so NSS database paths resolve
/// correctly even when root's `$HOME` is `/root`.
fn effective_home_dir() -> Option<PathBuf> {
    // Prefer the invoking user when running under sudo.
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if !sudo_user.is_empty() && sudo_user != "root" {
            if let Some(home) = passwd_home(&sudo_user) {
                return Some(home);
            }
        }
    }
    // Fall back to $HOME, then dirs::home_dir().
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

/// Parse `/etc/passwd` to find the home directory for `username`.
fn passwd_home(username: &str) -> Option<PathBuf> {
    let content = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in content.lines() {
        let mut fields = line.splitn(7, ':');
        let name = fields.next()?;
        if name != username {
            continue;
        }
        // Fields: name:pw:uid:gid:gecos:home:shell
        let home = fields.nth(4)?; // 0=pw,1=uid,2=gid,3=gecos,4=home
        return Some(PathBuf::from(home));
    }
    None
}

fn detect_ca_tool() -> LinuxCaTool {
    if command_exists("update-ca-certificates") {
        LinuxCaTool::UpdateCaCertificates
    } else if command_exists("update-ca-trust") {
        LinuxCaTool::UpdateCaTrust
    } else {
        LinuxCaTool::None
    }
}

fn find_certutil() -> Option<PathBuf> {
    // Absolute paths common on Debian/Ubuntu and RHEL.
    for candidate in &["/usr/bin/certutil", "/usr/local/bin/certutil"] {
        let p = Path::new(candidate);
        if p.exists() {
            return Some(p.to_path_buf());
        }
    }
    // Fall back to PATH lookup.
    let ok = matches!(
        std::process::Command::new("sh")
            .args(["-c", "command -v certutil >/dev/null 2>&1"])
            .status(),
        Ok(s) if s.success()
    );
    ok.then_some(PathBuf::from("certutil"))
}
