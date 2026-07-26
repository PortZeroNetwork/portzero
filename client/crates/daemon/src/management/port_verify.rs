//! Verify that a given PID has a socket in LISTEN state on a given port.

/// Returns `true` if `pid` has a TCP socket in LISTEN state on `port`.
pub fn pid_is_listening_on(pid: u32, port: u16) -> bool {
    pid_is_listening_impl(pid, port)
}

#[cfg(target_os = "linux")]
fn pid_is_listening_impl(pid: u32, port: u16) -> bool {
    let fd_dir = format!("/proc/{pid}/fd");
    let Ok(entries) = std::fs::read_dir(&fd_dir) else {
        tracing::warn!("port_verify: cannot inspect {fd_dir}");
        return false;
    };
    let mut owned_inodes = std::collections::HashSet::<u64>::new();
    for entry in entries {
        let Ok(entry) = entry else {
            tracing::warn!("port_verify: failed to read an entry in {fd_dir}");
            return false;
        };
        let Ok(target) = std::fs::read_link(entry.path()) else {
            tracing::warn!("port_verify: failed to inspect {}", entry.path().display());
            return false;
        };
        let target = target.to_string_lossy();
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|value| value.strip_suffix(']'))
            .and_then(|value| value.parse().ok())
        {
            owned_inodes.insert(inode);
        }
    }

    if owned_inodes.is_empty() {
        return false;
    }

    // /proc/<pid>/net is namespace-wide, so a matching port is accepted only
    // when its socket inode also appears in this PID's fd table.
    for suffix in &["tcp", "tcp6"] {
        let path = format!("/proc/{}/net/{}", pid, suffix);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!("port_verify: failed to inspect {path}: {error}");
                return false;
            }
        };
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // local_address: column 1; state: column 3; inode: column 9.
            if fields.len() < 10 {
                continue;
            }
            if !fields[3].eq_ignore_ascii_case("0A") {
                continue;
            }
            let Ok(inode) = fields[9].parse::<u64>() else {
                continue;
            };
            if !owned_inodes.contains(&inode) {
                continue;
            }
            let local_addr = fields[1];
            if let Some(colon) = local_addr.rfind(':') {
                let port_hex = &local_addr[colon + 1..];
                if let Ok(p) = u16::from_str_radix(port_hex, 16) {
                    if p == port {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn pid_is_listening_impl(pid: u32, port: u16) -> bool {
    use std::process::Command;

    let output = match Command::new("lsof")
        .args([
            "-nP",
            &format!("-iTCP:{}", port),
            "-sTCP:LISTEN",
            "-p",
            &pid.to_string(),
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("port_verify: lsof failed: {e}");
            return false;
        }
    };

    output.status.success() && !output.stdout.is_empty()
}

#[cfg(target_os = "windows")]
fn pid_is_listening_impl(pid: u32, port: u16) -> bool {
    use std::process::Command;

    let output = match Command::new("netstat").args(["-ano"]).output() {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("port_verify: netstat failed: {e}");
            return false;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let port_suffix = format!(":{}", port);
    for line in stdout.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Protocol  LocalAddr  ForeignAddr  State  PID
        if fields.len() < 5 {
            continue;
        }
        if fields[0].eq_ignore_ascii_case("TCP")
            && fields[3].eq_ignore_ascii_case("LISTENING")
            && fields[1].ends_with(&port_suffix)
        {
            if let Ok(p) = fields[4].parse::<u32>() {
                if p == pid {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn pid_is_listening_impl(_pid: u32, _port: u16) -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn current_process_owns_its_listener_but_not_another_process_listener() {
        // Spawn the child BEFORE binding. `Command::spawn` forks, and until the
        // exec completes the child's /proc/<pid>/fd is a copy of ours — so a
        // listener created first can appear in the child's fd table, and this
        // test read that as "the child is listening on our port". Creating the
        // socket after the fork makes it impossible for the child to hold that
        // inode, so the assertion tests process ownership rather than a race
        // against exec.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 2"])
            .spawn()
            .unwrap();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        assert!(pid_is_listening_on(std::process::id(), port));
        assert!(!pid_is_listening_on(child.id(), port));

        child.kill().unwrap();
        child.wait().unwrap();
    }
}
