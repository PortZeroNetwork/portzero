//! Windows trust-store installation.
//!
//! The CA is added to the `Root` certificate store — `LocalMachine\Root` when
//! running as the machine/SYSTEM account, otherwise `CurrentUser\Root` — via the
//! CryptoAPI. Firefox and other NSS-based browsers keep their own store, updated
//! separately through Mozilla's `certutil.exe` (deliberately excluding the
//! built-in `%SystemRoot%\System32\certutil.exe`, which is unrelated).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::common::{
    certutil_delete_args, find_nss_dbs_windows, install_nss_cert, is_windows_system_certutil,
    nss_db_contains_ca, run_command, run_command_capture, CERT_NICKNAME,
};
use super::TrustVerificationReport;

#[derive(Debug)]
pub(crate) struct WindowsTrustEnv {
    /// Absolute path to Mozilla NSS `certutil.exe`, if available.
    ///
    /// This intentionally excludes Windows' built-in `%SystemRoot%\System32\certutil.exe`.
    pub(crate) certutil_path: Option<PathBuf>,
    /// All NSS database directories found in the user's profile.
    pub(crate) nss_db_dirs: Vec<PathBuf>,
}

pub(super) fn install_impl(ca_cert_path: &Path) -> Result<()> {
    let use_machine_store = crate::is_windows_system_account();
    install_windows_root_store(ca_cert_path, use_machine_store)?;
    let env = detect_windows_trust_env();
    install_nss_dbs_windows(ca_cert_path, &env);
    Ok(())
}

pub(super) fn uninstall_impl() -> Result<()> {
    let use_machine_store = crate::is_windows_system_account();
    uninstall_windows_root_store(use_machine_store)?;
    let env = detect_windows_trust_env();
    uninstall_nss_dbs_windows(&env);
    Ok(())
}

pub(super) fn verify_installation_impl(ca_cert_path: &Path) -> Result<TrustVerificationReport> {
    let env = detect_windows_trust_env();
    let mut missing = Vec::new();

    let store = WindowsCertStore::open_root(crate::is_windows_system_account())?;
    if !store.contains_subject(CERT_NICKNAME)? {
        missing.push(if crate::is_windows_system_account() {
            "Windows LocalMachine\\Root store".to_string()
        } else {
            "Windows CurrentUser\\Root store".to_string()
        });
    }

    if env.nss_db_dirs.is_empty() {
        tracing::debug!("verify_installation: no Windows NSS databases found to check");
    } else if let Some(certutil) = env.certutil_path.as_deref() {
        for db_dir in &env.nss_db_dirs {
            if !nss_db_contains_ca(certutil, db_dir) {
                missing.push(format!("Windows NSS database {}", db_dir.display()));
            }
        }
    } else {
        missing.push(format!(
            "{} Windows NSS database(s) were found but Mozilla certutil.exe is not available",
            env.nss_db_dirs.len()
        ));
    }

    if !ca_cert_path.exists() {
        missing.push(format!("CA certificate file {}", ca_cert_path.display()));
    }

    Ok(TrustVerificationReport { missing })
}

fn install_windows_root_store(ca_cert_path: &Path, use_machine_store: bool) -> Result<()> {
    let der = read_first_pem_cert_der(ca_cert_path)?;
    let store = WindowsCertStore::open_root(use_machine_store)?;
    // Remove any previously installed PortZero CA anchors first. CERT_STORE_ADD_
    // REPLACE_EXISTING only replaces a byte-identical cert, so regenerating the
    // CA (e.g. the legacy → name-constrained migration) would otherwise leave
    // the old, broadly-trusted anchor behind alongside the new one.
    match store.delete_by_subject(CERT_NICKNAME) {
        Ok(count) if count > 0 => {
            tracing::info!("Windows: removed {count} stale PortZero CA anchor(s) before install")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("Windows: could not remove stale PortZero CA anchors: {e:#}"),
    }
    store.add_encoded_cert(&der)?;
    tracing::info!(
        "Windows: installed CA cert to {}",
        if use_machine_store {
            "LocalMachine\\Root"
        } else {
            "CurrentUser\\Root"
        }
    );
    Ok(())
}

fn uninstall_windows_root_store(use_machine_store: bool) -> Result<()> {
    let store = WindowsCertStore::open_root(use_machine_store)?;
    match store.delete_by_subject(CERT_NICKNAME) {
        Ok(count) if count > 0 => {
            tracing::info!(
                "Windows: removed {count} CA cert(s) from {}",
                if use_machine_store {
                    "LocalMachine\\Root"
                } else {
                    "CurrentUser\\Root"
                }
            )
        }
        Ok(_) => tracing::info!(
            "Windows: CA cert was not present in {}",
            if use_machine_store {
                "LocalMachine\\Root"
            } else {
                "CurrentUser\\Root"
            }
        ),
        Err(e) => tracing::warn!(
            "Windows: failed to remove CA from {}: {e:#}",
            if use_machine_store {
                "LocalMachine\\Root"
            } else {
                "CurrentUser\\Root"
            }
        ),
    }
    Ok(())
}

fn install_nss_dbs_windows(ca_cert_path: &Path, env: &WindowsTrustEnv) {
    let Some(ref certutil) = env.certutil_path else {
        if !env.nss_db_dirs.is_empty() {
            tracing::warn!(
                "Windows: NSS databases found ({} location(s)) but Mozilla certutil.exe is not \
                 installed or was not found on PATH. Firefox may not trust *.portzero.local. \
                 Install NSS tools and ensure Mozilla's certutil.exe appears before \
                 C:\\Windows\\System32\\certutil.exe on PATH.",
                env.nss_db_dirs.len()
            );
        }
        return;
    };
    for db_dir in &env.nss_db_dirs {
        if let Err(e) = install_nss_cert(certutil, db_dir, ca_cert_path) {
            tracing::warn!(
                "Windows: failed to add CA to NSS DB {}: {e:#}",
                db_dir.display()
            );
        } else {
            tracing::info!("Windows: added CA to NSS DB {}", db_dir.display());
        }
    }
}

fn uninstall_nss_dbs_windows(env: &WindowsTrustEnv) {
    let Some(ref certutil) = env.certutil_path else {
        return;
    };
    for db_dir in &env.nss_db_dirs {
        let args = certutil_delete_args(db_dir);
        if let Err(e) = run_command(certutil.to_str().unwrap_or("certutil"), &args) {
            tracing::warn!(
                "Windows: failed to remove CA from NSS DB {}: {e:#}",
                db_dir.display()
            );
        } else {
            tracing::info!("Windows: removed CA from NSS DB {}", db_dir.display());
        }
    }
}

fn detect_windows_trust_env() -> WindowsTrustEnv {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    WindowsTrustEnv {
        certutil_path: find_mozilla_certutil_windows(),
        nss_db_dirs: find_nss_dbs_windows(home.as_deref(), appdata.as_deref()),
    }
}

fn find_mozilla_certutil_windows() -> Option<PathBuf> {
    executable_candidates_on_path("certutil.exe")
        .into_iter()
        .find(|candidate| is_mozilla_certutil_windows(candidate))
}

fn executable_candidates_on_path(name: &str) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return vec![];
    };
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .filter(|candidate| candidate.is_file())
        .collect()
}

fn is_mozilla_certutil_windows(path: &Path) -> bool {
    if is_windows_system_certutil(path) {
        return false;
    }
    let mut command = std::process::Command::new(path);
    command.arg("-H");
    // A wedged certutil.exe on PATH must not hang detection; on timeout the
    // watchdog kills it and we treat the candidate as "not usable" and skip it.
    let Ok(output) = run_command_capture(command, &format!("certutil -H probe {}", path.display()))
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase()
        + &String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    text.contains("nss") || text.contains("certificate database") || text.contains("-a add")
}

fn read_first_pem_cert_der(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let cert = rustls_pemfile::certs(&mut reader)
        .next()
        .transpose()
        .context("parse CA cert PEM")?
        .ok_or_else(|| anyhow::anyhow!("no certificate found in {}", path.display()))?;
    Ok(cert.as_ref().to_vec())
}

struct WindowsCertStore(windows_sys::Win32::Security::Cryptography::HCERTSTORE);

impl WindowsCertStore {
    fn open_root(use_machine_store: bool) -> Result<Self> {
        use windows_sys::Win32::Security::Cryptography::{
            CertOpenStore, CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_A,
            CERT_SYSTEM_STORE_CURRENT_USER, CERT_SYSTEM_STORE_LOCAL_MACHINE, X509_ASN_ENCODING,
        };

        let flags = if use_machine_store {
            CERT_SYSTEM_STORE_LOCAL_MACHINE
        } else {
            CERT_SYSTEM_STORE_CURRENT_USER
        } | CERT_STORE_OPEN_EXISTING_FLAG;

        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_A,
                X509_ASN_ENCODING,
                0,
                flags,
                c"ROOT".as_ptr().cast(),
            )
        };
        if store.is_null() {
            return Err(std::io::Error::last_os_error()).context(if use_machine_store {
                "open LocalMachine\\Root store"
            } else {
                "open CurrentUser\\Root store"
            });
        }
        Ok(Self(store))
    }

    fn add_encoded_cert(&self, der: &[u8]) -> Result<()> {
        use windows_sys::Win32::Security::Cryptography::{
            CertAddEncodedCertificateToStore, CertFreeCertificateContext,
            CERT_STORE_ADD_REPLACE_EXISTING, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
        };

        let mut added = std::ptr::null_mut();
        let ok = unsafe {
            CertAddEncodedCertificateToStore(
                self.0,
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                der.as_ptr(),
                der.len()
                    .try_into()
                    .context("certificate DER is too large for Windows CryptoAPI")?,
                CERT_STORE_ADD_REPLACE_EXISTING,
                &mut added,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("add CA cert to CurrentUser\\Root store");
        }
        if !added.is_null() {
            unsafe {
                CertFreeCertificateContext(added);
            }
        }
        Ok(())
    }

    fn delete_by_subject(&self, subject: &str) -> Result<usize> {
        use windows_sys::Win32::Security::Cryptography::{
            CertDeleteCertificateFromStore, CertFindCertificateInStore, CERT_FIND_SUBJECT_STR_W,
            PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
        };

        let subject_wide = wide_null(subject);
        let mut deleted = 0;
        loop {
            let context = unsafe {
                CertFindCertificateInStore(
                    self.0,
                    X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                    0,
                    CERT_FIND_SUBJECT_STR_W,
                    subject_wide.as_ptr().cast(),
                    std::ptr::null(),
                )
            };
            if context.is_null() {
                return Ok(deleted);
            }
            let ok = unsafe { CertDeleteCertificateFromStore(context) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("delete CA cert from CurrentUser\\Root store");
            }
            deleted += 1;
        }
    }

    fn contains_subject(&self, subject: &str) -> Result<bool> {
        use windows_sys::Win32::Security::Cryptography::{
            CertFindCertificateInStore, CertFreeCertificateContext, CERT_FIND_SUBJECT_STR_W,
            PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
        };

        let subject_wide = wide_null(subject);
        let context = unsafe {
            CertFindCertificateInStore(
                self.0,
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                0,
                CERT_FIND_SUBJECT_STR_W,
                subject_wide.as_ptr().cast(),
                std::ptr::null(),
            )
        };
        if context.is_null() {
            return Ok(false);
        }
        unsafe {
            CertFreeCertificateContext(context);
        }
        Ok(true)
    }
}

impl Drop for WindowsCertStore {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Security::Cryptography::CertCloseStore(self.0, 0);
        }
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
