//! CLI handlers for CA certificate generation and OS trust store management.

use portzero_daemon::tls::{trust, LocalCa};

/// Generate the local CA certificate and key, persisting them to the platform
/// data directory.  Safe to call multiple times — existing certs are kept.
/// Does not require elevated privileges.
///
/// If a legacy CA generated before name constraints were added is found, it is
/// regenerated in place (atomically) so it is scoped to `*.portzero.local`. The
/// user is then told to re-run `trust install`, because the OS trust store
/// still holds the old, unconstrained anchor until they do.
pub fn generate() -> anyhow::Result<()> {
    let regenerated = LocalCa::ensure_name_constrained()?;
    let path = LocalCa::ca_cert_path()?;
    if regenerated {
        println!("Replaced the local CA with a name-constrained one (scoped to *.portzero.local).");
        println!(
            "Re-run `sudo portzero trust install` to trust the new CA; the old one is no longer used."
        );
    }
    println!("CA certificate ready: {}", path.display());
    Ok(())
}

/// Install the local CA into the OS trust store so browsers and system tools
/// accept `*.portzero.local` without certificate warnings.
///
/// Requires elevated privileges for system-wide trust on Linux (`sudo -E`) and
/// macOS (`sudo`). On Linux, running as the regular user still updates writable
/// per-user browser NSS stores.
pub fn install() -> anyhow::Result<()> {
    let path = LocalCa::ca_cert_path()?;
    if !path.exists() {
        anyhow::bail!(
            "CA certificate not found at {}. Run `portzero trust generate` first (without sudo).",
            path.display()
        );
    }
    trust::install(&path)?;
    println!("CA certificate installed to available trust stores.");
    Ok(())
}

/// Remove the local CA from the OS trust store.
///
/// Requires elevated privileges on Linux and macOS.
pub fn uninstall() -> anyhow::Result<()> {
    trust::uninstall()?;
    println!("CA certificate removed from system trust store.");
    Ok(())
}
