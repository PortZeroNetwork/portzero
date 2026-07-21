//! TLS / local-CA trust diagnostics.
//!
//! Split out of `checks.rs`, which had grown past the 1000-line budget these
//! checks are meant to be readable within. This is the "is the local CA present
//! and actually trusted by the stores that matter" cluster; `checks.rs` keeps
//! the process, state-file, and OS-networking checks.

#[cfg(target_os = "linux")]
use std::path::PathBuf;

use super::{Diagnostic, Fix, FixKind, Severity};
use crate::tls::{trust, LocalCa};

pub(super) fn check_ca_cert_exists() -> Option<Diagnostic> {
    match crate::tls::ca::LocalCa::ca_cert_path() {
        Ok(path) if !path.exists() => {
            tracing::debug!(
                "check_ca_cert_exists: CA cert missing at {}",
                path.display()
            );
            Some(Diagnostic {
                id: "ca_cert_missing".into(),
                severity: Severity::Error,
                category: "tls".into(),
                title: "Local CA certificate file is missing".to_string(),
                detail: "The wildcard cert for *.portzero.local cannot be served.".to_string(),
                fix: Some(Fix {
                    kind: FixKind::Confirm,
                    description: "Restart the daemon to regenerate the CA certificate".to_string(),
                    command: Some("portzero restart".to_string()),
                }),
            })
        }
        Err(e) => {
            tracing::warn!("check_ca_cert_exists: could not determine CA cert path: {e}");
            None
        }
        Ok(_) => None,
    }
}

pub(super) fn check_local_ca_trust_installation() -> Option<Diagnostic> {
    let ca_path = match LocalCa::ca_cert_path() {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!(
                "check_local_ca_trust_installation: could not determine CA cert path: {e}"
            );
            return None;
        }
    };

    if !ca_path.exists() {
        return None;
    }

    let report = match trust::verify_installation(&ca_path) {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!("check_local_ca_trust_installation: trust verification failed: {e:#}");
            return Some(Diagnostic {
                id: "ca_trust_verification_failed".into(),
                severity: Severity::Warning,
                category: "tls".into(),
                title: "Could not verify local CA installation".to_string(),
                detail: format!(
                    "PortZero could not inspect the trust stores that should contain {}.",
                    ca_path.display()
                ),
                fix: Some(Fix {
                    kind: FixKind::Manual,
                    description: "Run `portzero trust generate && portzero trust install` again."
                        .to_string(),
                    command: Some(
                        "portzero trust generate && sudo portzero trust install".to_string(),
                    ),
                }),
            });
        }
    };

    if report.is_clean() {
        return None;
    }

    Some(Diagnostic {
        id: "ca_trust_missing".into(),
        severity: Severity::Warning,
        category: "tls".into(),
        title: "Local CA trust installation is incomplete".to_string(),
        detail: format!(
            "The PortZero CA is missing from: {}.",
            report.missing.join(", ")
        ),
        fix: Some(Fix {
            kind: FixKind::Manual,
            description: "Reinstall the local CA into the trust stores.".to_string(),
            command: Some("portzero trust generate && sudo portzero trust install".to_string()),
        }),
    })
}

#[cfg(target_os = "linux")]
pub(super) fn check_snap_brave_tls_trust() -> Option<Diagnostic> {
    let paths = detected_snap_brave_paths();
    if paths.is_empty() {
        return None;
    }

    Some(Diagnostic {
        id: "snap_brave_tls_trust".into(),
        severity: Severity::Warning,
        category: "tls".into(),
        title: "Snap Brave may not trust the local PortZero CA".to_string(),
        detail: format!(
            "Snap Brave was detected at {}. Some Snap Brave/Chromium builds ignore locally installed CAs even when the PortZero CA is present in the system and NSS trust stores. If only Snap Brave shows net::ERR_CERT_AUTHORITY_INVALID for https://portzero.local, use the native Brave package or another non-Snap browser.",
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        fix: Some(Fix {
            kind: FixKind::Manual,
            description:
                "Use the native Brave package for trusted https://*.portzero.local browsing."
                    .to_string(),
            command: None,
        }),
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn check_snap_brave_tls_trust() -> Option<Diagnostic> {
    None
}

#[cfg(target_os = "linux")]
fn detected_snap_brave_paths() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/snap/bin/brave"),
        PathBuf::from("/var/lib/snapd/snap/bin/brave"),
        PathBuf::from("/snap/brave/current"),
    ];

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join("snap/brave/current"));
    }

    existing_paths(candidates)
}

#[cfg(target_os = "linux")]
pub(super) fn existing_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>()
}
