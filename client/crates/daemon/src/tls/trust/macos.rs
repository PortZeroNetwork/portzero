//! macOS trust-store installation.
//!
//! Two independent stores are provisioned:
//!
//! 1. **System Keychain** (`/Library/Keychains/System.keychain`) — covers
//!    Safari, Chrome, curl, and all other tools that delegate to Secure
//!    Transport. Uses `security add-trusted-cert -d -r trustRoot`. Requires
//!    the root privileges the daemon already holds for TUN device creation.
//!
//! 2. **Firefox NSS databases** — Firefox maintains its own NSS cert store
//!    independent of the system keychain. Added via `certutil` from the `nss`
//!    Homebrew formula (`brew install nss`). Best-effort with a warning if absent.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::common::{
    command_exists, delete_nss_cert, find_nss_dbs_macos, install_nss_cert, nss_db_contains_ca,
    run_command, CERT_NICKNAME,
};
use super::TrustVerificationReport;

/// Snapshot of the macOS trust environment, captured so detection is a pure
/// function testable without touching the real system.
#[derive(Debug)]
pub(crate) struct MacosTrustEnv {
    /// Absolute path to `certutil`, if available (`brew install nss`).
    pub(crate) certutil_path: Option<PathBuf>,
    /// All Firefox NSS database directories found in the user's home.
    pub(crate) nss_db_dirs: Vec<PathBuf>,
}

pub(super) fn install_impl(ca_cert_path: &Path) -> Result<()> {
    install_system_keychain(ca_cert_path)?;
    let env = detect_macos_trust_env();
    install_nss_dbs_macos(ca_cert_path, &env);
    Ok(())
}

pub(super) fn uninstall_impl() -> Result<()> {
    uninstall_system_keychain()?;
    let env = detect_macos_trust_env();
    uninstall_nss_dbs_macos(&env);
    Ok(())
}

pub(super) fn verify_installation_impl(ca_cert_path: &Path) -> Result<TrustVerificationReport> {
    let env = detect_macos_trust_env();
    let mut missing = Vec::new();

    if !system_keychain_contains_ca()? {
        missing.push("macOS system keychain".to_string());
    }

    if env.nss_db_dirs.is_empty() {
        tracing::debug!("verify_installation: no Firefox NSS databases found to check");
    } else if let Some(certutil) = env.certutil_path.as_deref() {
        for db_dir in &env.nss_db_dirs {
            if !nss_db_contains_ca(certutil, db_dir) {
                missing.push(format!("Firefox NSS database {}", db_dir.display()));
            }
        }
    } else {
        missing.push(format!(
            "{} Firefox NSS database(s) were found but certutil is not available",
            env.nss_db_dirs.len()
        ));
    }

    // Keep the signature consistent with other platforms and ensure the CA
    // file still exists when verification is called.
    if !ca_cert_path.exists() {
        missing.push(format!("CA certificate file {}", ca_cert_path.display()));
    }

    Ok(TrustVerificationReport { missing })
}

fn install_system_keychain(ca_cert_path: &Path) -> Result<()> {
    // Remove any previously installed PortZero CA anchors first. The keychain is
    // keyed by cert identity, not common name, so regenerating the CA (e.g. the
    // legacy → name-constrained migration) would otherwise leave the old,
    // broadly-trusted anchor behind alongside the new one.
    remove_existing_system_keychain_certs();

    let cert_str = ca_cert_path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("CA cert path is not valid UTF-8"))?;
    run_command(
        "security",
        &[
            "add-trusted-cert",
            "-d",
            "-r",
            "trustRoot",
            "-k",
            "/Library/Keychains/System.keychain",
            cert_str,
        ],
    )
    .context("security add-trusted-cert")?;
    tracing::info!("macOS: installed CA cert to system keychain");

    // Sweep again, now by hash, keeping only the anchor we just installed.
    // The pre-install sweep above deletes by common name and cannot tell one
    // PortZero CA from another, so it silently leaves everything behind
    // whenever `security delete-certificate` fails — which it does on macOS
    // when the System keychain declines a non-interactive trust edit. Observed
    // in the wild: three trusted `PortZero Local CA` anchors on one machine,
    // two of them predating name constraints and therefore trusted for *any*
    // host. Those are inert without their private keys (the CA key is
    // generated in memory and never persisted, see tls::ca), but an
    // accumulating pile of unconstrained roots in a user's system store is not
    // something to leave to chance.
    purge_superseded_anchors(ca_cert_path);
    Ok(())
}

/// Delete every trusted `PortZero Local CA` anchor whose SHA-256 differs from
/// the one at `keep_path`. Deleting by hash is precise, so unlike the
/// delete-by-name sweep this cannot remove the anchor we depend on.
fn purge_superseded_anchors(keep_path: &Path) {
    let Some(keep) = sha256_of_cert(keep_path) else {
        return;
    };
    let stale: Vec<String> = system_keychain_ca_hashes()
        .into_iter()
        .filter(|h| !h.eq_ignore_ascii_case(&keep))
        .collect();
    if stale.is_empty() {
        return;
    }

    let mut left = Vec::new();
    for hash in stale {
        if run_command(
            "security",
            &[
                "delete-certificate",
                "-Z",
                &hash,
                "-t",
                "/Library/Keychains/System.keychain",
            ],
        )
        .is_ok()
        {
            tracing::info!("macOS: removed superseded PortZero CA anchor {hash}");
        } else {
            left.push(hash);
        }
    }

    if !left.is_empty() {
        // Actionable rather than silent: these stay trusted until removed, and
        // the user is the only one who can authorize it if the daemon could not.
        tracing::warn!(
            "macOS: {} superseded PortZero CA anchor(s) are still trusted and could not be \
             removed automatically. Remove them with:\n{}",
            left.len(),
            left.iter()
                .map(|h| format!(
                    "  sudo security delete-certificate -Z {h} -t /Library/Keychains/System.keychain"
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

/// SHA-256 fingerprints of every `PortZero Local CA` in the System keychain.
fn system_keychain_ca_hashes() -> Vec<String> {
    let Ok(output) = std::process::Command::new("security")
        .args([
            "find-certificate",
            "-a",
            "-Z",
            "-c",
            CERT_NICKNAME,
            "/Library/Keychains/System.keychain",
        ])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("SHA-256 hash:"))
        .map(|h| h.trim().to_string())
        .collect()
}

/// SHA-256 fingerprint of a PEM cert on disk as uppercase hex with no
/// separators, matching the format `security find-certificate -Z` prints.
/// Shells out to `openssl` (already required elsewhere in this module's
/// workflow) rather than pulling a digest crate in for one call.
fn sha256_of_cert(path: &Path) -> Option<String> {
    let output = std::process::Command::new("openssl")
        .arg("x509")
        .arg("-in")
        .arg(path)
        .args(["-noout", "-fingerprint", "-sha256"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // e.g. "sha256 Fingerprint=CF:7A:C1:..." -> "CF7AC1..."
    let text = String::from_utf8_lossy(&output.stdout);
    let (_, hex) = text.split_once('=')?;
    let hex: String = hex
        .trim()
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if hex.is_empty() {
        return None;
    }
    Some(hex.to_ascii_uppercase())
}

/// Best-effort removal of every "PortZero Local CA" anchor from the system
/// keychain. `security delete-certificate` removes one match per invocation, so
/// loop until none remain (bounded to avoid spinning on a persistent failure).
fn remove_existing_system_keychain_certs() {
    for _ in 0..16 {
        if !system_keychain_contains_ca().unwrap_or(false) {
            return;
        }
        if run_command(
            "security",
            &[
                "delete-certificate",
                "-c",
                CERT_NICKNAME,
                "-t",
                "/Library/Keychains/System.keychain",
            ],
        )
        .is_err()
        {
            // Nothing left to delete, or the delete failed — stop and let the
            // add proceed; a leftover duplicate is not fatal.
            return;
        }
        tracing::info!("macOS: removed a stale PortZero CA anchor from system keychain");
    }
}

fn system_keychain_contains_ca() -> Result<bool> {
    let output = std::process::Command::new("security")
        .args([
            "find-certificate",
            "-c",
            CERT_NICKNAME,
            "-a",
            "/Library/Keychains/System.keychain",
        ])
        .output()
        .context("spawn security find-certificate")?;
    Ok(output.status.success())
}

fn uninstall_system_keychain() -> Result<()> {
    // Gracefully ignore failures — the cert may not be present.
    match run_command(
        "security",
        &[
            "delete-certificate",
            "-c",
            CERT_NICKNAME,
            "-t",
            "/Library/Keychains/System.keychain",
        ],
    ) {
        Ok(()) => tracing::info!("macOS: removed CA cert from system keychain"),
        Err(e) => tracing::warn!(
            "macOS: could not remove CA from system keychain (may not be present): {e:#}"
        ),
    }
    Ok(())
}

fn install_nss_dbs_macos(ca_cert_path: &Path, env: &MacosTrustEnv) {
    let Some(ref certutil) = env.certutil_path else {
        if !env.nss_db_dirs.is_empty() {
            tracing::warn!(
                "macOS: Firefox NSS databases found ({} location(s)) but certutil is not \
                 installed. Firefox may not trust *.portzero.local. \
                 Fix: brew install nss",
                env.nss_db_dirs.len()
            );
        }
        return;
    };
    for db_dir in &env.nss_db_dirs {
        if let Err(e) = install_nss_cert(certutil, db_dir, ca_cert_path) {
            tracing::warn!(
                "macOS: failed to add CA to NSS DB {}: {e:#}",
                db_dir.display()
            );
        } else {
            tracing::info!("macOS: added CA to NSS DB {}", db_dir.display());
        }
    }
}

fn uninstall_nss_dbs_macos(env: &MacosTrustEnv) {
    let Some(ref certutil) = env.certutil_path else {
        return;
    };
    for db_dir in &env.nss_db_dirs {
        if let Err(e) = delete_nss_cert(certutil, db_dir) {
            tracing::warn!(
                "macOS: failed to remove CA from NSS DB {}: {e:#}",
                db_dir.display()
            );
        } else {
            tracing::info!("macOS: removed CA from NSS DB {}", db_dir.display());
        }
    }
}

fn detect_macos_trust_env() -> MacosTrustEnv {
    MacosTrustEnv {
        certutil_path: find_certutil_macos(),
        nss_db_dirs: dirs::home_dir()
            .map(|h| find_nss_dbs_macos(&h))
            .unwrap_or_default(),
    }
}

/// Search for `certutil` in locations populated by `brew install nss`.
fn find_certutil_macos() -> Option<PathBuf> {
    for candidate in &[
        "/opt/homebrew/bin/certutil", // Homebrew Apple Silicon
        "/usr/local/bin/certutil",    // Homebrew Intel
    ] {
        let p = Path::new(candidate);
        if p.exists() {
            return Some(p.to_path_buf());
        }
    }
    // Last-resort PATH lookup.
    command_exists("certutil").then_some(PathBuf::from("certutil"))
}
