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
pub fn pid_is_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
    #[cfg(target_os = "macos")]
    {
        // kill(pid, 0) returns 0 if the process exists and we have permission to signal it.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
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
