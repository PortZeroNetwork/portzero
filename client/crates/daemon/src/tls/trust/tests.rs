use super::common::*;
use std::path::{Path, PathBuf};

// --- System CA paths ---

#[test]
fn debian_cert_dest_is_correct() {
    let dest = system_cert_dest_debian();
    assert_eq!(
        dest,
        PathBuf::from("/usr/share/ca-certificates/portzero/portzero-local-ca.crt")
    );
}

#[test]
fn debian_ssl_cert_path_is_snap_readable_cert_location() {
    assert_eq!(
        debian_ssl_cert_path(),
        PathBuf::from("/etc/ssl/certs/portzero-local-ca.pem")
    );
}

#[test]
fn rhel_cert_dest_is_correct() {
    let dest = system_cert_dest_rhel();
    assert_eq!(
        dest,
        PathBuf::from("/etc/pki/ca-trust/source/anchors/portzero-local-ca.crt")
    );
}

// --- certutil argument builders ---

#[test]
fn certutil_add_args_format() {
    let db = PathBuf::from("/home/user/.pki/nssdb");
    let cert = PathBuf::from("/tmp/ca.crt");
    let args = certutil_add_args(&db, &cert);
    assert_eq!(args[0], "-A");
    assert_eq!(args[1], "-d");
    assert_eq!(args[2], "sql:/home/user/.pki/nssdb");
    assert_eq!(args[3], "-t");
    assert_eq!(args[4], "C,,", "trust flags: TLS server auth only");
    assert_eq!(args[5], "-n");
    assert_eq!(args[6], CERT_NICKNAME);
    assert_eq!(args[7], "-i");
    assert_eq!(args[8], "/tmp/ca.crt");
}

#[test]
fn certutil_delete_args_format() {
    let db = PathBuf::from("/home/user/.pki/nssdb");
    let args = certutil_delete_args(&db);
    assert_eq!(args[0], "-D");
    assert_eq!(args[1], "-d");
    assert_eq!(args[2], "sql:/home/user/.pki/nssdb");
    assert_eq!(args[3], "-n");
    assert_eq!(args[4], CERT_NICKNAME);
}

#[test]
fn certutil_init_args_uses_empty_password_sql_store() {
    let db = PathBuf::from("/home/user/.pki/nssdb");
    let args = certutil_init_args(&db);
    assert_eq!(args[0], "-N");
    assert_eq!(args[1], "-d");
    assert_eq!(args[2], "sql:/home/user/.pki/nssdb");
    assert_eq!(args[3], "--empty-password");
}

#[test]
fn certutil_read_only_error_is_detected() {
    assert!(certutil_output_is_read_only(
        "certutil: function failed: SEC_ERROR_READ_ONLY: security library: read-only database."
    ));
    assert!(certutil_output_is_read_only(
        "security library: read-only database"
    ));
    assert!(!certutil_output_is_read_only(
        "certutil: could not find certificate named PortZero Local CA"
    ));
}

#[test]
fn certutil_sudo_args_prefix_program() {
    let args = certutil_delete_args(&PathBuf::from("/home/user/.pki/nssdb"));
    let sudo_args = certutil_sudo_args("/usr/bin/certutil", &args);
    assert_eq!(sudo_args[0], "/usr/bin/certutil");
    assert_eq!(sudo_args[1], "-D");
    assert_eq!(sudo_args[2], "-d");
    assert_eq!(sudo_args[3], "sql:/home/user/.pki/nssdb");
}

#[test]
fn p11_kit_anchor_path_is_etc_trust_source() {
    assert_eq!(
        p11_kit_anchor_path(),
        PathBuf::from("/etc/ca-certificates/trust-source/anchors/portzero-local-ca.crt")
    );
}

#[test]
fn certutil_add_uses_sql_prefix() {
    let db = PathBuf::from("/home/user/.mozilla/firefox/abc123.default");
    let cert = PathBuf::from("/tmp/ca.crt");
    let args = certutil_add_args(&db, &cert);
    assert!(
        args[2].starts_with("sql:"),
        "NSS SQL store requires sql: prefix"
    );
}

// --- NSS DB discovery ---

#[test]
fn find_nss_dbs_picks_up_pki_nssdb() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let nssdb = home.join(".pki/nssdb");
    std::fs::create_dir_all(&nssdb).unwrap();
    std::fs::write(nssdb.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs(home);
    assert!(found.contains(&nssdb), "should find ~/.pki/nssdb");
}

#[test]
fn native_chromium_nss_db_is_shared_by_native_brave() {
    let home = PathBuf::from("/home/user");
    assert_eq!(
        native_chromium_nss_db(&home),
        PathBuf::from("/home/user/.pki/nssdb")
    );
}

#[test]
fn find_nss_dbs_picks_up_firefox_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let profile = home.join(".mozilla/firefox/abc123.default-release");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs(home);
    assert!(
        found.contains(&profile),
        "should find Firefox profile NSS DB"
    );
}

#[test]
fn find_nss_dbs_picks_up_snap_brave_nssdb() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let nssdb = home.join("snap/brave/current/.pki/nssdb");
    std::fs::create_dir_all(&nssdb).unwrap();
    std::fs::write(nssdb.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs(home);
    assert!(found.contains(&nssdb), "should find Snap Brave NSS DB");
}

#[test]
fn find_nss_dbs_picks_up_snap_brave_revision_nssdb() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let nssdb = home.join("snap/brave/646/.pki/nssdb");
    std::fs::create_dir_all(&nssdb).unwrap();
    std::fs::write(nssdb.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs(home);
    assert!(
        found.contains(&nssdb),
        "should find versioned Snap Brave NSS DB"
    );
}

#[test]
fn find_nss_dbs_picks_up_snap_firefox_common_profile() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let profile = home.join("snap/firefox/common/.mozilla/firefox/abc123.default");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs(home);
    assert!(
        found.contains(&profile),
        "should find Firefox Snap common profile NSS DB"
    );
}

#[test]
fn find_nss_dbs_picks_up_flatpak_brave_nssdb() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let nssdb = home.join(".var/app/com.brave.Browser/.pki/nssdb");
    std::fs::create_dir_all(&nssdb).unwrap();
    std::fs::write(nssdb.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs(home);
    assert!(found.contains(&nssdb), "should find Flatpak Brave NSS DB");
}

#[test]
fn find_nss_dbs_ignores_dirs_without_cert9_db() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    // A Firefox profile dir without cert9.db — should be ignored.
    let profile = home.join(".mozilla/firefox/xyz.default");
    std::fs::create_dir_all(&profile).unwrap();

    assert!(find_nss_dbs(home).is_empty());
}

#[test]
fn find_nss_dbs_empty_when_no_browsers_installed() {
    let dir = tempfile::tempdir().unwrap();
    assert!(find_nss_dbs(dir.path()).is_empty());
}

#[test]
fn find_nss_dbs_ignores_legacy_cert8_db() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let nssdb = home.join(".pki/nssdb");
    std::fs::create_dir_all(&nssdb).unwrap();
    // Only cert8.db present — we require cert9.db (SQL store).
    std::fs::write(nssdb.join("cert8.db"), b"").unwrap();

    assert!(find_nss_dbs(home).is_empty());
}

// --- passwd home parsing ---

// --- macOS NSS DB discovery ---

#[test]
fn find_nss_dbs_macos_picks_up_firefox_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let profile = home.join("Library/Application Support/Firefox/Profiles/abc123.default-release");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs_macos(home);
    assert!(
        found.contains(&profile),
        "should find Firefox profile NSS DB"
    );
}

#[test]
fn find_nss_dbs_macos_picks_up_dev_edition() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let profile =
        home.join("Library/Application Support/Firefox Developer Edition/Profiles/dev.default");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs_macos(home);
    assert!(
        found.contains(&profile),
        "should find Firefox Dev Edition NSS DB"
    );
}

#[test]
fn find_nss_dbs_macos_picks_up_nightly() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let profile = home.join("Library/Application Support/Firefox Nightly/Profiles/nightly.default");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs_macos(home);
    assert!(
        found.contains(&profile),
        "should find Firefox Nightly NSS DB"
    );
}

#[test]
fn find_nss_dbs_macos_empty_when_no_firefox() {
    let dir = tempfile::tempdir().unwrap();
    assert!(find_nss_dbs_macos(dir.path()).is_empty());
}

#[test]
fn find_nss_dbs_macos_ignores_profile_without_cert9_db() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let profile = home.join("Library/Application Support/Firefox/Profiles/no-cert.default");
    std::fs::create_dir_all(&profile).unwrap();
    // No cert9.db written.

    assert!(find_nss_dbs_macos(home).is_empty());
}

#[test]
fn find_nss_dbs_windows_picks_up_firefox_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let appdata = dir.path().join("AppData/Roaming");
    let profile = appdata.join("Mozilla/Firefox/Profiles/abc123.default-release");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs_windows(None, Some(&appdata));
    assert!(
        found.contains(&profile),
        "should find Windows Firefox NSS DB"
    );
}

#[test]
fn find_nss_dbs_windows_picks_up_pki_nssdb() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let nssdb = home.join(".pki/nssdb");
    std::fs::create_dir_all(&nssdb).unwrap();
    std::fs::write(nssdb.join("cert9.db"), b"").unwrap();

    let found = find_nss_dbs_windows(Some(home), None);
    assert!(found.contains(&nssdb), "should find Windows ~/.pki/nssdb");
}

#[test]
fn find_nss_dbs_windows_ignores_profile_without_cert9_db() {
    let dir = tempfile::tempdir().unwrap();
    let appdata = dir.path().join("AppData/Roaming");
    let profile = appdata.join("Mozilla/Firefox/Profiles/no-cert.default");
    std::fs::create_dir_all(&profile).unwrap();

    assert!(find_nss_dbs_windows(None, Some(&appdata)).is_empty());
}

#[test]
fn windows_system_certutil_detection_matches_builtin_paths() {
    assert!(is_windows_system_certutil(Path::new(
        r"C:\Windows\System32\certutil.exe"
    )));
    assert!(is_windows_system_certutil(Path::new(
        r"C:\Windows\SysWOW64\certutil.exe"
    )));
    assert!(!is_windows_system_certutil(Path::new(
        r"C:\Program Files\NSS\bin\certutil.exe"
    )));
}

#[cfg(target_os = "linux")]
#[test]
fn passwd_home_finds_user() {
    let dir = tempfile::tempdir().unwrap();
    let passwd = dir.path().join("passwd");
    std::fs::write(
        &passwd,
        "root:x:0:0:root:/root:/bin/bash\nalice:x:1000:1000:Alice:/home/alice:/bin/bash\n",
    )
    .unwrap();
    let content = std::fs::read_to_string(&passwd).unwrap();
    // Inline the parsing logic against our test file.
    let home: Option<PathBuf> = content.lines().find_map(|line| {
        let mut fields = line.splitn(7, ':');
        let name = fields.next()?;
        if name != "alice" {
            return None;
        }
        let home = fields.nth(4)?;
        Some(PathBuf::from(home))
    });
    assert_eq!(home, Some(PathBuf::from("/home/alice")));
}

// --- Command timeout watchdog (task-69) ---
//
// These exercise the cross-platform primitive that guarantees no external
// trust-install command can wedge the daemon. They run on Unix hosts using a
// real `sleep`; the same wrapper guards the Windows `certutil` calls, which
// cannot be exercised from this environment.

#[cfg(unix)]
#[test]
fn run_command_capture_kills_child_that_exceeds_timeout() {
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    let start = std::time::Instant::now();
    let result =
        run_command_capture_with_timeout(cmd, std::time::Duration::from_millis(300), "sleep 30");
    let elapsed = start.elapsed();

    assert!(result.is_err(), "a child exceeding the timeout must error");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("did not complete") && msg.contains("sleep 30"),
        "error should name the wedged command: {msg}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "caller must return promptly after the timeout, took {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn run_command_capture_returns_output_for_fast_command() {
    let mut cmd = std::process::Command::new("sh");
    cmd.args(["-c", "printf hello"]);
    let output =
        run_command_capture_with_timeout(cmd, std::time::Duration::from_secs(10), "printf")
            .expect("fast command should complete within the timeout");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"hello");
}

#[cfg(unix)]
#[test]
fn run_command_capture_reports_spawn_failure() {
    let cmd = std::process::Command::new("/nonexistent/portzero-does-not-exist");
    let result =
        run_command_capture_with_timeout(cmd, std::time::Duration::from_secs(10), "missing binary");
    assert!(result.is_err(), "spawning a missing binary must error");
}
