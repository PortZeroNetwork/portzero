//! Resolve a TCP source port to the owning process PID.

/// Return the PID of the process that owns the TCP socket with the given source port,
/// or `None` if it cannot be determined.
pub fn pid_for_source_port(source_port: u16) -> Option<u32> {
    pid_for_source_port_impl(source_port)
}

#[cfg(target_os = "linux")]
fn pid_for_source_port_impl(source_port: u16) -> Option<u32> {
    let inode = find_inode_for_port(source_port)?;
    find_pid_for_inode(inode)
}

/// Search /proc/net/tcp and /proc/net/tcp6 for a socket whose local port matches.
/// Returns the inode number if found.
#[cfg(target_os = "linux")]
fn find_inode_for_port(source_port: u16) -> Option<u64> {
    for path in &["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines().skip(1) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                // Column index 1: local_address (IP:PORT in hex, little-endian for IPv4)
                // Column index 9: inode
                if fields.len() < 10 {
                    continue;
                }
                let local_addr = fields[1];
                // local_addr is like "0F02000A:1F90"
                if let Some(colon) = local_addr.rfind(':') {
                    let port_hex = &local_addr[colon + 1..];
                    if let Ok(port) = u16::from_str_radix(port_hex, 16) {
                        if port == source_port {
                            if let Ok(inode) = fields[9].parse::<u64>() {
                                return Some(inode);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Walk /proc/*/fd/ looking for a symlink `socket:[inode]` and return the PID.
#[cfg(target_os = "linux")]
fn find_pid_for_inode(inode: u64) -> Option<u32> {
    let target = format!("socket:[{}]", inode);
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("pid_lookup: cannot read /proc: {e}");
            return None;
        }
    };

    for entry in proc_dir.flatten() {
        let fname = entry.file_name();
        let name = fname.to_string_lossy();
        // Only numeric entries are PIDs
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let fd_dir = entry.path().join("fd");
        let fd_entries = match std::fs::read_dir(&fd_dir) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for fd_entry in fd_entries.flatten() {
            if let Ok(link) = std::fs::read_link(fd_entry.path()) {
                if link.to_string_lossy() == target {
                    return Some(pid);
                }
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn pid_for_source_port_impl(source_port: u16) -> Option<u32> {
    use std::process::Command;

    // lsof -nP -iTCP -sTCP:ESTABLISHED prints lines like:
    //   COMMAND   PID  USER  FD  TYPE  DEVICE  SIZE/OFF  NODE  NAME
    //   daemon   1234  root  7u  IPv4  ...               TCP  127.0.0.1:PORT->...
    let output = match Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:ESTABLISHED"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("pid_lookup: lsof failed: {e}");
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let port_suffix = format!(":{}", source_port);
    for line in stdout.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // NAME column is last; check for local addr containing our port
        if fields.len() < 9 {
            continue;
        }
        let name = fields[8];
        // NAME is "local->remote"; local part is before "->"
        let local = name.split("->").next().unwrap_or("");
        if local.ends_with(&port_suffix) {
            if let Ok(pid) = fields[1].parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn pid_for_source_port_impl(source_port: u16) -> Option<u32> {
    use std::process::Command;

    // netstat -ano prints lines like:
    //   TCP  127.0.0.1:PORT  0.0.0.0:0  ESTABLISHED  PID
    let output = match Command::new("netstat").args(["-ano"]).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("pid_lookup: netstat failed: {e}");
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let port_suffix = format!(":{}", source_port);
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Expected: Protocol  LocalAddr  ForeignAddr  State  PID
        if fields.len() < 5 {
            continue;
        }
        if fields[0].eq_ignore_ascii_case("TCP")
            && fields[3].eq_ignore_ascii_case("ESTABLISHED")
            && fields[1].ends_with(&port_suffix)
        {
            if let Ok(pid) = fields[4].parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn pid_for_source_port_impl(_source_port: u16) -> Option<u32> {
    None
}

/// Return `true` if a process with the given PID is currently running.
/// Find a running PortZero daemon that the state file does not account for.
///
/// The PID file is written by whichever daemon started last, and
/// [`read_daemon_pid`](crate::discovery_loop::read_daemon_pid) deletes it when
/// that PID is dead. So a daemon this user cannot manage — typically the root
/// LaunchDaemon/systemd unit — reads back as "no daemon at all". Callers that
/// act on that conclusion (the tray, the app, `doctor`) then start another one,
/// which is how a machine ends up with several.
///
/// Returns the PID of a live `portzero start` process other than this one, so a
/// caller can say "running, but not the instance recorded here" instead of
/// "not running". Best-effort: an empty result only means none was found.
#[cfg(unix)]
pub fn find_unmanaged_daemon() -> Option<u32> {
    let me = std::process::id();
    let out = std::process::Command::new("pgrep")
        .args(["-f", "portzero start"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .find(|&pid| pid != me && pid_is_alive(pid))
}

#[cfg(not(unix))]
pub fn find_unmanaged_daemon() -> Option<u32> {
    None
}

pub fn pid_is_alive(pid: u32) -> bool {
    // PID 0 is never a real process to probe, and it is what a truncated or
    // corrupt pidfile reads back as. It must not be reported alive: on macOS
    // POSIX gives `kill(0, sig)` the special meaning "every process in the
    // caller's process group", so the probe below would succeed and the caller
    // would treat its own stale lock as permanently held.
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        // `/proc/<n>` is stat-able for a *thread* id as well as a process id —
        // thread directories are hidden from `readdir` but resolve fine when
        // named directly. A bare `.exists()` therefore reports a dead PID as
        // alive as soon as any unrelated process spawns a thread that happens
        // to be assigned that id, which is common on a busy desktop: a real
        // install got wedged when `/usr/bin/kaccess` took the tray's old PID
        // for its `QXcbEventQueue` thread. Requiring `Tgid == pid` keeps only
        // thread-group leaders, i.e. actual processes.
        linux_tgid(pid) == Some(pid)
    }
    #[cfg(target_os = "macos")]
    {
        // kill(pid, 0) returns 0 if the process exists and we have permission
        // to signal it. Convert rather than cast: a pid past i32::MAX would
        // wrap negative, which POSIX reads as "the process GROUP -pid".
        match libc::pid_t::try_from(pid) {
            Ok(p) => unsafe { libc::kill(p, 0) == 0 },
            Err(_) => false,
        }
    }
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if !h.is_null() {
                CloseHandle(h);
                true
            } else {
                false
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = pid;
        true
    }
}

/// Thread-group id of `/proc/<pid>`, or `None` if there is no such task.
///
/// Equals `pid` for a process and the owning process's id for a thread, which
/// is how [`pid_is_alive`] tells the two apart.
#[cfg(target_os = "linux")]
fn linux_tgid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Tgid:"))
        .and_then(|v| v.trim().parse().ok())
}

/// Return `true` if `pid` is a live process **and** the binary it is running is
/// named `expected_stem` (no directory, no `.exe` suffix — e.g.
/// `"portzero-tray"`).
///
/// Callers that read a PID out of a file are really asking "is the component
/// that wrote this still running", and a bare PID cannot answer that: PIDs are
/// recycled, so a record left behind by a crashed component eventually names
/// some unrelated process and reads as alive forever. That is not a rare race —
/// it wedged a real install, where a dead tray's PID was reused and the
/// single-instance guard then refused to start a tray at every login while
/// `portzero doctor` and `portzero version` both reported the tray as running.
///
/// Verifying the binary name closes that off: an unrelated process fails the
/// match, and the one case that still passes — a *different* process running
/// the same binary — is genuinely the answer the caller wants.
///
/// Falls back to a plain liveness probe on platforms where the running binary
/// cannot be identified, which is no worse than the PID-only check it replaces.
pub fn process_is_alive_named(pid: u32, expected_stem: &str) -> bool {
    if pid == 0 {
        return false;
    }
    match running_binary_stem(pid) {
        Some(stem) => stem == expected_stem,
        // On Linux a truncated `comm` is still authoritative for names that fit
        // (all of ours do); elsewhere an unreadable name means "can't tell", so
        // fall back rather than declaring a live component dead and starting a
        // duplicate.
        None => pid_is_alive(pid),
    }
}

/// File name of the executable `pid` is running, minus any `.exe` suffix, or
/// `None` if the process is gone or its binary cannot be identified.
pub fn running_binary_stem(pid: u32) -> Option<String> {
    let path = running_binary_path(pid)?;
    let name = std::path::Path::new(&path).file_name()?.to_str()?;
    Some(name.strip_suffix(".exe").unwrap_or(name).to_string())
}

#[cfg(target_os = "linux")]
fn running_binary_path(pid: u32) -> Option<String> {
    if linux_tgid(pid) != Some(pid) {
        return None;
    }
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(exe) => {
            let exe = exe.to_str()?;
            // An in-place upgrade unlinks the old binary while the old process
            // keeps running it, and the kernel then appends " (deleted)" to the
            // symlink target. Strip it, or a still-running pre-upgrade
            // component reads as "some other binary" and we start a duplicate.
            Some(exe.strip_suffix(" (deleted)").unwrap_or(exe).to_string())
        }
        // `/proc/<pid>/exe` needs ptrace-level access, so it is unreadable for
        // another user's process (and under a restrictive `ptrace_scope`).
        // `comm` is world-readable; it is truncated to 15 bytes, which every
        // PortZero binary name fits inside.
        Err(_) => std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty()),
    }
}

#[cfg(target_os = "macos")]
fn running_binary_path(pid: u32) -> Option<String> {
    // `comm` is the executable path, which for our bundled components is
    // `…/PortZero Tray.app/Contents/MacOS/portzero-tray`; the caller takes the
    // file name. `ps` prints nothing at all for a PID that is not running.
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!path.is_empty()).then_some(path)
}

#[cfg(target_os = "windows")]
fn running_binary_path(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // Extended-length path limit, so a component installed under a deep path
    // still reports its name instead of failing the buffer.
    const PATH_BUF: usize = 32_768;
    // `PROCESS_NAME_FORMAT` value 0 = `PROCESS_NAME_WIN32` (a Win32 path,
    // rather than the `\Device\…` native form).
    const PROCESS_NAME_WIN32: u32 = 0;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = vec![0u16; PATH_BUF];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn running_binary_path(_pid: u32) -> Option<String> {
    None
}

#[cfg(test)]
mod liveness_tests {
    use super::*;

    #[test]
    fn pid_zero_is_never_alive() {
        assert!(!pid_is_alive(0));
        assert!(!process_is_alive_named(0, "portzero-tray"));
    }

    #[test]
    fn our_own_process_is_alive() {
        assert!(pid_is_alive(std::process::id()));
    }

    /// The test binary is not named `portzero-tray`, so a name-checked probe
    /// against our own live PID must still say "not the tray" — this is exactly
    /// the PID-reuse case that used to read as alive.
    #[test]
    fn a_live_process_running_a_different_binary_does_not_match() {
        assert!(!process_is_alive_named(std::process::id(), "portzero-tray"));
    }

    #[test]
    fn a_live_process_matches_its_own_binary_name() {
        let stem = running_binary_stem(std::process::id());
        // Only meaningful where we can identify the running binary at all.
        if let Some(stem) = stem {
            assert!(process_is_alive_named(std::process::id(), &stem));
        }
    }

    /// `/proc/<tid>` resolves for threads too, which is how a dead component's
    /// recycled PID used to look alive. Spawn a thread and check that its id is
    /// not mistaken for a process.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_thread_id_is_not_a_live_process() {
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            // SAFETY: plain syscall with no arguments and no memory effects.
            let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
            tx.send(tid).unwrap();
            // Hold the thread open until the assertions have run.
            let _ = done_rx.recv();
        });

        let tid = rx.recv().unwrap();
        assert_ne!(tid, std::process::id(), "expected a distinct thread id");
        assert!(
            std::path::Path::new(&format!("/proc/{tid}")).exists(),
            "precondition: /proc/<tid> resolves, which is the trap being guarded"
        );
        assert!(!pid_is_alive(tid), "a thread id must not read as a process");
        assert!(!process_is_alive_named(tid, "portzero-tray"));

        let _ = done_tx.send(());
        handle.join().unwrap();
    }
}
