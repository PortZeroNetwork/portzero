//! Cross-platform helpers shared by the per-platform trust-store installers.
//!
//! The pure path/argument builders and NSS-database discovery functions are
//! `pub(crate)` and compiled on every platform (guarded by `allow(dead_code)`
//! where a given platform does not use them) so they can be unit-tested without
//! touching the real system.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub(crate) const CERT_NICKNAME: &str = "PortZero Local CA";
const SYSTEM_CERT_NAME: &str = "portzero-local-ca.crt";

/// Destination path for the CA cert on Debian/Ubuntu.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn system_cert_dest_debian() -> PathBuf {
    PathBuf::from("/usr/share/ca-certificates")
        .join("portzero")
        .join(SYSTEM_CERT_NAME)
}

/// Previous Debian/Ubuntu destination. Snap applications can see the symlink
/// under `/etc/ssl/certs` but cannot follow it into `/usr/local/share`, so keep
/// this only for migration cleanup.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn legacy_system_cert_dest_debian() -> PathBuf {
    PathBuf::from("/usr/local/share/ca-certificates").join(SYSTEM_CERT_NAME)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn debian_ssl_cert_path() -> PathBuf {
    PathBuf::from("/etc/ssl/certs/portzero-local-ca.pem")
}

/// Destination path for the CA cert on RHEL/Fedora.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn system_cert_dest_rhel() -> PathBuf {
    PathBuf::from("/etc/pki/ca-trust/source/anchors").join(SYSTEM_CERT_NAME)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn p11_kit_anchor_path() -> PathBuf {
    PathBuf::from("/etc/ca-certificates/trust-source/anchors").join(SYSTEM_CERT_NAME)
}

/// `certutil` arguments to add the CA to an NSS database directory.
/// The trust flags `"C,,"` mean: trusted for TLS server auth, not for email or
/// object signing.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn certutil_add_args(db_dir: &Path, cert_path: &Path) -> Vec<String> {
    vec![
        "-A".to_string(),
        "-d".to_string(),
        format!("sql:{}", db_dir.display()),
        "-t".to_string(),
        "C,,".to_string(),
        "-n".to_string(),
        CERT_NICKNAME.to_string(),
        "-i".to_string(),
        cert_path.display().to_string(),
    ]
}

/// `certutil` arguments to delete the CA from an NSS database directory.
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn certutil_delete_args(db_dir: &Path) -> Vec<String> {
    vec![
        "-D".to_string(),
        "-d".to_string(),
        format!("sql:{}", db_dir.display()),
        "-n".to_string(),
        CERT_NICKNAME.to_string(),
    ]
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]
pub(crate) fn certutil_init_args(db_dir: &Path) -> Vec<String> {
    vec![
        "-N".to_string(),
        "-d".to_string(),
        format!("sql:{}", db_dir.display()),
        "--empty-password".to_string(),
    ]
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn certutil_sudo_args(program: &str, args: &[String]) -> Vec<String> {
    let mut sudo_args = Vec::with_capacity(args.len() + 1);
    sudo_args.push(program.to_string());
    sudo_args.extend(args.iter().cloned());
    sudo_args
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn certutil_list_args(db_dir: &Path) -> Vec<String> {
    vec![
        "-L".to_string(),
        "-d".to_string(),
        format!("sql:{}", db_dir.display()),
    ]
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn certutil_output_is_read_only(output: &str) -> bool {
    output.contains("SEC_ERROR_READ_ONLY")
        || output.to_ascii_lowercase().contains("read-only database")
}

/// Collect all NSS database directories found under `home`.
///
/// Checked locations:
/// - `~/.pki/nssdb` (Chrome, Chromium, native Brave)
/// - `~/.mozilla/firefox/*/`                                      (Firefox)
/// - `~/.var/app/org.mozilla.firefox/.mozilla/firefox/*/`         (Firefox Flatpak)
/// - `~/snap/firefox/current/.mozilla/firefox/*/`                 (Firefox Snap)
/// - `~/snap/firefox/common/.mozilla/firefox/*/`                  (Firefox Snap)
/// - `~/snap/{brave,chromium}/{current,<revision>}/.pki/nssdb`    (Snap browsers)
/// - `~/.var/app/{com.brave.Browser,org.chromium.Chromium}/.pki/nssdb`
///   (Flatpak browsers)
///
/// A directory is included only if it contains a `cert9.db` file (the SQLite
/// NSS store). Legacy `cert8.db` (Berkeley DB) is intentionally ignored; all
/// modern installs use the SQL store.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn find_nss_dbs(home: &Path) -> Vec<PathBuf> {
    let mut dbs = Vec::new();

    // Chrome / Chromium / native Brave — direct NSS DB directory.
    push_nss_db_if_exists(&mut dbs, native_chromium_nss_db(home));

    // Firefox-family profile directories — enumerate subdirs for cert9.db.
    let firefox_bases = [
        home.join(".mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
        home.join("snap/firefox/current/.mozilla/firefox"),
        home.join("snap/firefox/common/.mozilla/firefox"),
    ];
    for base in &firefox_bases {
        dbs.extend(nss_dbs_under(base));
    }

    // Sandboxed Chromium-family browsers keep a private NSS store instead of
    // sharing ~/.pki/nssdb.
    let chromium_nss_dbs = [
        home.join(".var/app/com.brave.Browser/.pki/nssdb"),
        home.join(".var/app/org.chromium.Chromium/.pki/nssdb"),
    ];
    for db in chromium_nss_dbs {
        push_nss_db_if_exists(&mut dbs, db);
    }
    dbs.extend(snap_chromium_nss_dbs(home, "brave"));
    dbs.extend(snap_chromium_nss_dbs(home, "chromium"));

    dbs
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn native_chromium_nss_db(home: &Path) -> PathBuf {
    home.join(".pki/nssdb")
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn push_nss_db_if_exists(dbs: &mut Vec<PathBuf>, db: PathBuf) {
    if db.join("cert9.db").exists() && !dbs.contains(&db) {
        dbs.push(db);
    }
}

/// Return subdirectories of `dir` that contain a `cert9.db` file.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn nss_dbs_under(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("cert9.db").exists())
        .collect()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn snap_chromium_nss_dbs(home: &Path, snap_name: &str) -> Vec<PathBuf> {
    let base = home.join("snap").join(snap_name);
    let Ok(entries) = std::fs::read_dir(base) else {
        return vec![];
    };

    let mut dbs = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let db = entry.path().join(".pki/nssdb");
        push_nss_db_if_exists(&mut dbs, db);
    }
    dbs
}

/// Collect all Firefox NSS database directories found under a macOS home dir.
///
/// Checked locations:
/// - `~/Library/Application Support/Firefox/Profiles/*/`
/// - `~/Library/Application Support/Firefox Developer Edition/Profiles/*/`
/// - `~/Library/Application Support/Firefox Nightly/Profiles/*/`
///
/// Chrome and Chromium on macOS delegate to the system keychain, not NSS,
/// so only Firefox-family installs need `certutil` treatment.
/// A directory is included only if it contains a `cert9.db` file.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn find_nss_dbs_macos(home: &Path) -> Vec<PathBuf> {
    let bases = [
        home.join("Library/Application Support/Firefox/Profiles"),
        home.join("Library/Application Support/Firefox Developer Edition/Profiles"),
        home.join("Library/Application Support/Firefox Nightly/Profiles"),
    ];
    let mut dbs = Vec::new();
    for base in &bases {
        dbs.extend(nss_dbs_under(base));
    }
    dbs
}

/// Collect all NSS database directories found under Windows profile roots.
///
/// Checked locations:
/// - `%APPDATA%\Mozilla\Firefox\Profiles\*`
/// - `%APPDATA%\Mozilla\Firefox Developer Edition\Profiles\*`
/// - `%APPDATA%\Mozilla\Firefox Nightly\Profiles\*`
/// - `%APPDATA%\LibreWolf\Profiles\*`
/// - `%USERPROFILE%\.pki\nssdb`
///
/// A directory is included only if it contains a `cert9.db` file.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn find_nss_dbs_windows(home: Option<&Path>, appdata: Option<&Path>) -> Vec<PathBuf> {
    let mut dbs = Vec::new();

    if let Some(appdata) = appdata {
        let bases = [
            appdata.join("Mozilla/Firefox/Profiles"),
            appdata.join("Mozilla/Firefox Developer Edition/Profiles"),
            appdata.join("Mozilla/Firefox Nightly/Profiles"),
            appdata.join("LibreWolf/Profiles"),
        ];
        for base in &bases {
            dbs.extend(nss_dbs_under(base));
        }
    }

    if let Some(home) = home {
        let pki = home.join(".pki/nssdb");
        if pki.join("cert9.db").exists() {
            dbs.push(pki);
        }
    }

    dbs
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn is_windows_system_certutil(path: &Path) -> bool {
    let lower = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    lower.ends_with("\\windows\\system32\\certutil.exe")
        || lower.ends_with("\\windows\\syswow64\\certutil.exe")
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn nss_db_contains_ca(certutil: &Path, db_dir: &Path) -> bool {
    let program = certutil.to_string_lossy().to_string();
    let args = certutil_list_args(db_dir);
    run_certutil_output(&program, &args)
        .map(|output| {
            output.status.success() && {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                stdout.contains(CERT_NICKNAME) || stderr.contains(CERT_NICKNAME)
            }
        })
        .unwrap_or(false)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn install_nss_cert(certutil: &Path, db_dir: &Path, ca_cert_path: &Path) -> Result<()> {
    let program = certutil.to_str().unwrap_or("certutil");
    ensure_nss_db_exists(program, db_dir)?;
    let add_args = certutil_add_args(db_dir, ca_cert_path);

    match run_certutil(program, &add_args) {
        Ok(()) => Ok(()),
        Err(first_err) if first_err.is_read_only() => run_certutil_sudo(program, &add_args)
            .with_context(|| {
                format!(
                    "add CA to read-only NSS DB {} after user certutil failed: {first_err}",
                    db_dir.display()
                )
            }),
        Err(first_err) => {
            let delete_args = certutil_delete_args(db_dir);
            match run_certutil(program, &delete_args) {
                Ok(()) => {}
                Err(delete_err) if delete_err.is_read_only() => {
                    run_certutil_sudo(program, &delete_args).with_context(|| {
                        format!(
                            "delete existing CA from read-only NSS DB {} after add failed: {first_err}",
                            db_dir.display()
                        )
                    })?;
                }
                Err(_) => {}
            }
            let retry_args = certutil_add_args(db_dir, ca_cert_path);
            match run_certutil(program, &retry_args) {
                Ok(()) => Ok(()),
                Err(retry_err) if retry_err.is_read_only() => {
                    run_certutil_sudo(program, &retry_args).with_context(|| {
                        format!(
                        "replace existing CA in read-only NSS DB {} after add failed: {first_err}",
                        db_dir.display()
                    )
                    })
                }
                Err(retry_err) => Err::<(), anyhow::Error>(retry_err.into()).with_context(|| {
                    format!("replace existing NSS cert after add failed: {first_err}")
                }),
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn ensure_nss_db_exists(program: &str, db_dir: &Path) -> Result<()> {
    if db_dir.join("cert9.db").exists() {
        return Ok(());
    }

    std::fs::create_dir_all(db_dir).with_context(|| format!("create {}", db_dir.display()))?;
    run_certutil(program, &certutil_init_args(db_dir))
        .with_context(|| format!("initialize NSS DB {}", db_dir.display()))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[cfg_attr(target_os = "windows", allow(dead_code))]
pub(crate) fn delete_nss_cert(certutil: &Path, db_dir: &Path) -> Result<()> {
    let program = certutil.to_str().unwrap_or("certutil");
    let args = certutil_delete_args(db_dir);
    match run_certutil(program, &args) {
        Ok(()) => Ok(()),
        Err(err) if err.is_read_only() => run_certutil_sudo(program, &args)
            .with_context(|| format!("delete CA from read-only NSS DB {}", db_dir.display())),
        Err(err) => Err(err.into()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug)]
struct CertutilError {
    program: String,
    status: Option<std::process::ExitStatus>,
    stdout: String,
    stderr: String,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl CertutilError {
    fn is_read_only(&self) -> bool {
        certutil_output_is_read_only(&self.stdout) || certutil_output_is_read_only(&self.stderr)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl std::fmt::Display for CertutilError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "{} exited with {}", self.program, status)?,
            None => write!(f, "failed to spawn {}", self.program)?,
        }
        if !self.stderr.trim().is_empty() {
            write!(f, ": {}", self.stderr.trim())?;
        } else if !self.stdout.trim().is_empty() {
            write!(f, ": {}", self.stdout.trim())?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl std::error::Error for CertutilError {}

/// Hard timeout applied to every external command the trust installer spawns.
///
/// The daemon runs trust installation inside a 30s overlay-startup budget (see
/// `OverlayNetwork::start_with_progress`). On Windows a single wedged child —
/// `certutil.exe -H` probing a hung NSS build, or an NSS `certutil` blocked on a
/// locked `cert9.db` — historically consumed that entire budget and the overlay
/// never came up (task-69). No individual command may block the daemon
/// indefinitely, so each one is spawned with this watchdog and killed if it
/// overruns.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
const TRUST_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawn `command` and wait up to [`TRUST_COMMAND_TIMEOUT`] for it to exit,
/// capturing stdout and stderr. If it overruns, the child is killed and an error
/// naming `label` is returned so the wedge site is identifiable in logs.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn run_command_capture(
    command: std::process::Command,
    label: &str,
) -> Result<std::process::Output> {
    run_command_capture_with_timeout(command, TRUST_COMMAND_TIMEOUT, label)
}

/// Spawn `command`, drain its output on background threads, and wait up to
/// `timeout` for it to exit. On timeout the child is killed and an error is
/// returned that names `label` (the phase/command, matching the daemon's
/// "overlay startup step: X" logging style) so a wedge is identifiable.
///
/// stdin is redirected to null so a tool that blocks waiting on console input
/// fails immediately rather than hanging until the watchdog fires.
///
/// Shared beyond trust installation: any subprocess in a daemon-loop path must
/// be bounded, because a single child that never closes its pipes wedges
/// `Command::output()` forever — seen with `docker inspect` whose stdout pipe
/// write-end leaked into the long-lived `docker events` stream child on macOS,
/// deadlocking the discovery loop (the pipe never EOFs even after the child
/// exits).
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn run_command_capture_with_timeout(
    mut command: std::process::Command,
    timeout: std::time::Duration,
    label: &str,
) -> Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().with_context(|| format!("spawn {label}"))?;

    // Drain stdout/stderr on background threads so a chatty child cannot deadlock
    // against a full pipe buffer while we poll for exit, and so the reads unblock
    // (the pipes close) the instant we kill a wedged child.
    let stdout_reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let start = std::time::Instant::now();
    let status = loop {
        match child
            .try_wait()
            .with_context(|| format!("wait for {label}"))?
        {
            Some(status) => break status,
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(
                        "trust install step timed out after {}s and was killed: {label}",
                        timeout.as_secs()
                    );
                    anyhow::bail!(
                        "{label} did not complete within {}s and was killed",
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    };

    let stdout = stdout_reader
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_reader
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run_certutil(program: &str, args: &[String]) -> std::result::Result<(), CertutilError> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    let output = run_command_capture(command, program).map_err(|e| CertutilError {
        program: program.to_string(),
        status: None,
        stdout: String::new(),
        stderr: e.to_string(),
    })?;
    if output.status.success() {
        return Ok(());
    }
    Err(CertutilError {
        program: program.to_string(),
        status: Some(output.status),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_certutil_sudo(program: &str, args: &[String]) -> Result<()> {
    run_command("sudo", &certutil_sudo_args(program, args))
}

#[cfg(target_os = "windows")]
fn run_certutil_sudo(program: &str, args: &[String]) -> Result<()> {
    run_command(program, args)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn run_certutil_output(program: &str, args: &[String]) -> Result<std::process::Output> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    run_command_capture(command, program)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub(crate) fn run_command(program: &str, args: &[impl AsRef<std::ffi::OsStr>]) -> Result<()> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    let output = run_command_capture(command, program)?;
    if !output.status.success() {
        anyhow::bail!("{program} exited with {}", output.status);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn command_exists(cmd: &str) -> bool {
    matches!(
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1")])
            .status(),
        Ok(s) if s.success()
    )
}
