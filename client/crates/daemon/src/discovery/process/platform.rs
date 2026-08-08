//! Per-process, OS-specific port enumeration and environment reading, plus
//! the pure parsers for their command/kernel output (kept host-agnostic so they
//! are unit-tested on every CI runner, not just their native platform).

#[allow(unused_imports)]
use crate::discovery::*;

/// Return all TCP ports the process is actively listening on, with bind addresses.
pub(in crate::discovery) fn discover_process_ports(pid: u32) -> Vec<ListeningPort> {
    #[cfg(target_os = "linux")]
    {
        return discover_ports_linux(pid);
    }

    #[cfg(target_os = "macos")]
    {
        return discover_ports_lsof(pid);
    }

    #[cfg(target_os = "windows")]
    {
        return discover_ports_windows(pid);
    }

    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(target_os = "linux")]
fn discover_ports_linux(pid: u32) -> Vec<ListeningPort> {
    // Collect socket inodes owned by this process via /proc/<pid>/fd/.
    //
    // IMPORTANT: only fall back to spawning `lsof` when /proc is actually
    // UNREADABLE (permissions), never merely because the process owns no
    // TCP listeners. Most processes on a system have no listening sockets,
    // and this function is called for every pid by
    // `enumerate_system_listeners` — conflating "empty" with "unreadable"
    // used to spawn one lsof per socketless process, hundreds of spawns per
    // scan, which wedged the discovery loop for tens of seconds (hosted CI)
    // to minutes (busy dev machines).
    let mut owned_inodes = std::collections::HashSet::new();
    let fd_dir = format!("/proc/{}/fd", pid);
    match std::fs::read_dir(&fd_dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Ok(target) = std::fs::read_link(entry.path()) {
                    let s = target.to_string_lossy();
                    if let Some(inode_str) =
                        s.strip_prefix("socket:[").and_then(|s| s.strip_suffix(']'))
                    {
                        if let Ok(inode) = inode_str.parse::<u64>() {
                            owned_inodes.insert(inode);
                        }
                    }
                }
            }
        }
        // fd dir unreadable (different user, no privilege) — lsof may still
        // see the process's sockets, so this one case keeps the fallback.
        Err(_) => return discover_ports_lsof(pid),
    }

    // Readable fd dir with no socket fds at all: the process has no sockets.
    // Definitive — no fallback.
    if owned_inodes.is_empty() {
        return Vec::new();
    }

    let tcp_path = format!("/proc/{}/net/tcp", pid);
    let tcp6_path = format!("/proc/{}/net/tcp6", pid);
    let mut ports = Vec::new();

    for path in [&tcp_path, &tcp6_path] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines().skip(1) {
                if let Some(p) = parse_proc_net_tcp_line(line, &owned_inodes) {
                    ports.push(p);
                }
            }
        }
    }

    // Sockets exist but none is a TCP listener (UDP/unix/connected sockets
    // parse to nothing): that, too, is a definitive answer from /proc — do
    // NOT spawn lsof, it would only re-derive the same result.
    ports
}

/// Parse one line from /proc/<pid>/net/tcp or tcp6.
///
/// Returns Some only for LISTEN (state 0A) sockets whose inode is in `owned`.
/// Columns: idx local_addr remote_addr state ... inode
#[cfg(target_os = "linux")]
pub(in crate::discovery) fn parse_proc_net_tcp_line(
    line: &str,
    owned: &std::collections::HashSet<u64>,
) -> Option<ListeningPort> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 10 {
        return None;
    }
    // state must be 0A (LISTEN)
    if parts[3] != "0A" {
        return None;
    }
    let inode: u64 = parts[9].parse().ok()?;
    if !owned.contains(&inode) {
        return None;
    }
    let local_addr = parts[1];
    let (addr_hex, port_hex) = local_addr.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if port == 0 {
        return None;
    }
    Some(ListeningPort::new(parse_proc_net_hex_addr(addr_hex)?, port))
}

/// Decode the hex address field of a `/proc/<pid>/net/tcp[6]` line.
///
/// The kernel prints each 32-bit word in **host** byte order, so on the
/// little-endian hosts portzero targets `0100007F` is `127.0.0.1` and the
/// 32-char form `00000000000000000000000001000000` is `::1`. IPv4-mapped
/// addresses (`::ffff:127.0.0.1`) are normalized to their IPv4 form so the
/// dial address matches the family the peer actually needs.
#[cfg(target_os = "linux")]
fn parse_proc_net_hex_addr(addr_hex: &str) -> Option<std::net::IpAddr> {
    fn word(hex: &str) -> Option<[u8; 4]> {
        Some(u32::from_str_radix(hex, 16).ok()?.to_le_bytes())
    }

    match addr_hex.len() {
        8 => Some(std::net::IpAddr::V4(std::net::Ipv4Addr::from(word(
            addr_hex,
        )?))),
        32 => {
            let mut octets = [0u8; 16];
            for (idx, chunk) in addr_hex.as_bytes().chunks(8).enumerate() {
                octets[idx * 4..idx * 4 + 4]
                    .copy_from_slice(&word(std::str::from_utf8(chunk).ok()?)?);
            }
            let v6 = std::net::Ipv6Addr::from(octets);
            Some(match v6.to_ipv4_mapped() {
                Some(v4) => std::net::IpAddr::V4(v4),
                None => std::net::IpAddr::V6(v6),
            })
        }
        _ => None,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn discover_ports_lsof(pid: u32) -> Vec<ListeningPort> {
    use std::process::Command;

    // `-a` ANDs the selection criteria. Without it, lsof ORs `-iTCP -sTCP:LISTEN`
    // with `-p <pid>`, returning EVERY listening TCP socket on the system unioned
    // with this pid's sockets — so a process would appear to listen on ports it
    // doesn't own (e.g. sshd's *:22), mismapping the service backend.
    let output = match Command::new("lsof")
        .args(["-a", "-iTCP", "-sTCP:LISTEN", "-nP", "-p", &pid.to_string()])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_lsof_stdout(&stdout)
}

/// Parse the full stdout of `lsof -a -iTCP -sTCP:LISTEN -nP -p <pid>` into the
/// set of listening ports. Pure (no process spawning) so it is unit-testable.
///
/// The first line is the `lsof` header (`COMMAND PID USER …`) and is skipped.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::discovery) fn parse_lsof_stdout(stdout: &str) -> Vec<ListeningPort> {
    stdout.lines().skip(1).filter_map(parse_lsof_line).collect()
}

/// Parse a single `lsof` output line into a [`ListeningPort`], if it carries a
/// listening address/port.
///
/// With `-sTCP:LISTEN` the NAME column is printed as e.g.
/// `127.0.0.1:50706 (LISTEN)`, so the trailing whitespace token is `(LISTEN)`
/// rather than the address. We therefore scan every whitespace token and pick
/// the address token = the one whose `rsplit_once(':')` yields a parseable
/// `u16` port. This naturally skips `(LISTEN)`, `TCP`, `0t0`, and other columns.
///
/// `lsof` renders a wildcard bind as a bare `*` for both families, so the
/// address alone cannot say whether `*:5173` is `0.0.0.0` or `::`. The TYPE
/// column (`IPv4` / `IPv6`) does, and is read here for exactly that case.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::discovery) fn parse_lsof_line(line: &str) -> Option<ListeningPort> {
    let is_v6 = line
        .split_whitespace()
        .any(|token| token.eq_ignore_ascii_case("IPv6"));

    for token in line.split_whitespace() {
        // token is like "*:8080", "127.0.0.1:50706", "[::]:8080", "[::1]:57889"
        let Some((addr, port_str)) = token.rsplit_once(':') else {
            continue;
        };
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };
        if port == 0 {
            continue;
        }
        return Some(ListeningPort::new(parse_listen_address(addr, is_v6), port));
    }
    None
}

/// Parse the address part of a listener address as printed by `lsof`, netstat,
/// or `Get-NetTCPConnection`: `*`, `0.0.0.0`, `::`, `127.0.0.1`, `[::1]`, or a
/// specific interface address, optionally carrying an IPv6 zone (`%lo0`).
///
/// `v6_hint` decides the family of a bare `*`. Anything that fails to parse
/// falls back to the wildcard of the hinted family, which reproduces the old
/// behavior of dialing loopback rather than dropping the listener entirely.
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos", target_os = "windows")),
    allow(dead_code)
)]
pub(in crate::discovery) fn parse_listen_address(addr: &str, v6_hint: bool) -> std::net::IpAddr {
    let wildcard = if v6_hint {
        std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
    } else {
        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
    };

    let trimmed = addr.trim();
    if trimmed.is_empty() || trimmed == "*" {
        return wildcard;
    }

    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(trimmed);
    // Drop an IPv6 zone index ("fe80::1%lo0"), which `IpAddr` will not parse.
    let zoneless = unbracketed
        .split_once('%')
        .map(|(addr, _zone)| addr)
        .unwrap_or(unbracketed);

    match zoneless.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => std::net::IpAddr::V4(v4),
            None => std::net::IpAddr::V6(v6),
        },
        Ok(ip) => ip,
        Err(_) => wildcard,
    }
}

#[cfg(target_os = "windows")]
fn discover_ports_windows(pid: u32) -> Vec<ListeningPort> {
    use std::process::Command;

    let script = format!(
        r#"
$ErrorActionPreference = 'SilentlyContinue'
Get-NetTCPConnection -State Listen -OwningProcess {pid} |
  ForEach-Object {{ "$($_.LocalAddress)|$($_.LocalPort)" }}
"#
    );

    let output = match Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return discover_ports_windows_netstat(pid);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let ports = parse_windows_tcp_connection_stdout(&stdout);
    if ports.is_empty() {
        discover_ports_windows_netstat(pid)
    } else {
        ports
    }
}

#[cfg(target_os = "windows")]
fn discover_ports_windows_netstat(pid: u32) -> Vec<ListeningPort> {
    use std::process::Command;

    let output = match Command::new("netstat").args(["-ano", "-p", "tcp"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_windows_netstat_stdout(&stdout, pid)
}

#[cfg(target_os = "windows")]
pub(in crate::discovery) fn discover_all_ports_windows_by_pid(
) -> std::collections::HashMap<u32, Vec<ListeningPort>> {
    use std::process::Command;

    let output = match Command::new("netstat").args(["-ano", "-p", "tcp"]).output() {
        Ok(o) if o.status.success() => o,
        _ => return std::collections::HashMap::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_windows_netstat_stdout_by_pid(&stdout)
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::discovery) fn parse_windows_tcp_connection_stdout(
    stdout: &str,
) -> Vec<ListeningPort> {
    stdout
        .lines()
        .filter_map(parse_windows_tcp_connection_line)
        .collect()
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::discovery) fn parse_windows_tcp_connection_line(line: &str) -> Option<ListeningPort> {
    let (addr, port_str) = line.trim().split_once('|')?;
    let port = port_str.trim().parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }

    // `Get-NetTCPConnection` prints the address unbracketed and never as `*`,
    // so it is always self-describing — no family hint needed.
    Some(ListeningPort::new(parse_listen_address(addr, false), port))
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::discovery) fn parse_windows_netstat_stdout(
    stdout: &str,
    pid: u32,
) -> Vec<ListeningPort> {
    stdout
        .lines()
        .filter_map(|line| parse_windows_netstat_line(line, pid))
        .collect()
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::discovery) fn parse_windows_netstat_stdout_by_pid(
    stdout: &str,
) -> std::collections::HashMap<u32, Vec<ListeningPort>> {
    let mut out: std::collections::HashMap<u32, Vec<ListeningPort>> =
        std::collections::HashMap::new();
    for line in stdout.lines() {
        if let Some((pid, port)) = parse_windows_netstat_line_any_pid(line) {
            out.entry(pid).or_default().push(port);
        }
    }
    out
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::discovery) fn parse_windows_netstat_line(
    line: &str,
    pid: u32,
) -> Option<ListeningPort> {
    let (line_pid, port) = parse_windows_netstat_line_any_pid(line)?;
    if line_pid == pid {
        Some(port)
    } else {
        None
    }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::discovery) fn parse_windows_netstat_line_any_pid(
    line: &str,
) -> Option<(u32, ListeningPort)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 5 || !parts[0].eq_ignore_ascii_case("TCP") {
        return None;
    }
    if !parts[3].eq_ignore_ascii_case("LISTENING") {
        return None;
    }

    let pid = parts[4].parse::<u32>().ok()?;
    let port = parse_windows_local_address_port(parts[1])?;
    Some((pid, port))
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::discovery) fn parse_windows_local_address_port(local: &str) -> Option<ListeningPort> {
    let (addr, port_str) = if let Some(rest) = local.strip_prefix('[') {
        let (addr, tail) = rest.split_once("]:")?;
        (addr, tail)
    } else {
        local.rsplit_once(':')?
    };

    let port = port_str.trim().parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }

    // netstat brackets IPv6 addresses, so the family is unambiguous here too.
    Some(ListeningPort::new(parse_listen_address(addr, false), port))
}

pub(in crate::discovery) fn scan_process_env(pid: u32, var_name: &str) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        scan_process_env_linux(pid, var_name)
    }
    #[cfg(target_os = "macos")]
    {
        scan_process_env_macos(pid, var_name)
    }
    #[cfg(target_os = "windows")]
    {
        scan_process_env_windows(pid, var_name)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (pid, var_name);
        None
    }
}

#[cfg(target_os = "linux")]
fn scan_process_env_linux(pid: u32, var_name: &str) -> Option<String> {
    let environ_path = format!("/proc/{}/environ", pid);
    let data = std::fs::read(&environ_path).ok()?;

    let prefix = format!("{}=", var_name);
    for entry in data.split(|&b| b == 0) {
        if let Ok(s) = std::str::from_utf8(entry) {
            if let Some(value) = s.strip_prefix(&prefix) {
                return Some(value.to_string());
            }
        }
    }

    None
}

// There are two ways to read another process's environment on macOS:
//
//   1. `ps -wwwE` (scan_process_env_macos_ps): shell out to `ps` and let it
//      print argv + env. Simple and needs no unsafe code — but on macOS 15.7+
//      (Sequoia) Apple stopped exposing process environments through `ps -E`
//      even for root, so it returns argv only and PZ_TUNNEL discovery finds
//      nothing (task-74).
//
//   2. sysctl(KERN_PROCARGS2) (scan_process_env_macos_procargs2): read the
//      kernel's own copy of the process's argv + env buffer directly. This is
//      the same source `ps` itself used to read; it still works on 15.7+ for
//      same-user/root callers and needs no subprocess. This is the active path.
//
// To fall back to the old behavior, swap the active call below for the
// commented one (and likewise in scan_macos_env_candidates for the batched
// scans).
#[cfg(target_os = "macos")]
fn scan_process_env_macos(pid: u32, var_name: &str) -> Option<String> {
    scan_process_env_macos_procargs2(pid, var_name)
    // scan_process_env_macos_ps(pid, var_name)
}

/// Read `var_name` from `pid`'s environment via sysctl(KERN_PROCARGS2). Returns
/// `None` if the process is gone, unreadable (different user, no privilege), or
/// does not set the variable. See scan_process_env_macos for the rationale.
#[cfg(target_os = "macos")]
fn scan_process_env_macos_procargs2(pid: u32, var_name: &str) -> Option<String> {
    let prefix = format!("{var_name}=");
    read_process_env_macos(pid)?
        .into_iter()
        .find_map(|entry| entry.strip_prefix(&prefix).map(str::to_string))
}

/// Return the full environment of `pid` as `KEY=VALUE` strings by reading the
/// kernel's KERN_PROCARGS2 buffer. `None` on any sysctl failure.
///
/// The KERN_PROCARGS2 buffer is laid out as:
///
/// ```text
///   [ argc: i32 ][ exec_path\0 ][ \0 padding ][ argv[0]\0 .. argv[argc-1]\0 ]
///   [ env[0]\0 .. env[n]\0 ][ apple[0]\0 .. ]
/// ```
///
/// We skip `argc`, the executable path, its zero padding, and the `argc` argv
/// strings; everything remaining is the environment (trailed by the harmless
/// `apple[]` strings, which also look like `KEY=VALUE`).
#[cfg(target_os = "macos")]
fn read_process_env_macos(pid: u32) -> Option<Vec<String>> {
    // Size the buffer from KERN_ARGMAX (the KERN_PROCARGS2 null-sizing call is
    // unreliable, so allocate the documented maximum up front).
    let mut argmax: libc::c_int = 0;
    let mut argmax_len = std::mem::size_of::<libc::c_int>();
    let mut argmax_mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let rc = unsafe {
        libc::sysctl(
            argmax_mib.as_mut_ptr(),
            argmax_mib.len() as libc::c_uint,
            &mut argmax as *mut _ as *mut libc::c_void,
            &mut argmax_len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || argmax <= 0 {
        return None;
    }

    let mut buf = vec![0u8; argmax as usize];
    let mut buf_len = buf.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut buf_len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(buf_len);

    parse_procargs2_env(&buf)
}

/// Extract the environment strings from a raw KERN_PROCARGS2 buffer. Split out
/// from the sysctl call so it can be unit-tested with synthetic buffers.
///
/// Pure byte parsing (only uses `libc::c_int` for sizing, an unconditional
/// dependency) — not gated to macOS so it can be exercised from unit tests on
/// any host, even though it is only ever called from macOS-specific code.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(in crate::discovery) fn parse_procargs2_env(buf: &[u8]) -> Option<Vec<String>> {
    if buf.len() < std::mem::size_of::<libc::c_int>() {
        return None;
    }
    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let mut pos = std::mem::size_of::<libc::c_int>();

    // Skip the executable path string and its trailing zero padding.
    while pos < buf.len() && buf[pos] != 0 {
        pos += 1;
    }
    while pos < buf.len() && buf[pos] == 0 {
        pos += 1;
    }

    // Skip the argc argv strings.
    let mut skipped = 0i32;
    while skipped < argc && pos < buf.len() {
        while pos < buf.len() && buf[pos] != 0 {
            pos += 1;
        }
        pos += 1; // step past the null terminator
        skipped += 1;
    }

    // Everything left is the environment (plus the apple[] strings).
    let mut env = Vec::new();
    while pos < buf.len() {
        let start = pos;
        while pos < buf.len() && buf[pos] != 0 {
            pos += 1;
        }
        if pos > start {
            if let Ok(s) = std::str::from_utf8(&buf[start..pos]) {
                env.push(s.to_string());
            }
        }
        pos += 1; // step past the null terminator
    }

    Some(env)
}

/// Old macOS env read via `ps -wwwE`. Broken on macOS 15.7+ (see
/// scan_process_env_macos); kept as the documented fallback and for parity with
/// how `ps` sourced the data historically.
#[cfg(target_os = "macos")]
#[allow(dead_code)]
fn scan_process_env_macos_ps(pid: u32, var_name: &str) -> Option<String> {
    use std::process::Command;

    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-wwwE", "-o", "command="])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let prefix = format!("{}=", var_name);

    for token in stdout.split_whitespace() {
        if let Some(value) = token.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn scan_process_env_windows(pid: u32, var_name: &str) -> Option<String> {
    let entries = read_windows_process_environment(pid).ok()?;
    let prefix = format!("{var_name}=");
    entries.into_iter().find_map(|entry| {
        if entry.len() >= prefix.len() && entry[..prefix.len()].eq_ignore_ascii_case(&prefix) {
            Some(entry[prefix.len()..].to_string())
        } else {
            None
        }
    })
}

#[cfg(target_os = "windows")]
fn read_windows_process_environment(pid: u32) -> Result<Vec<String>> {
    use anyhow::Context;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    #[repr(C)]
    struct ProcessBasicInformation {
        reserved1: *mut c_void,
        peb_base_address: *mut c_void,
        reserved2: [*mut c_void; 2],
        unique_process_id: usize,
        reserved3: *mut c_void,
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            process_handle: HANDLE,
            process_information_class: u32,
            process_information: *mut c_void,
            process_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    const PROCESS_BASIC_INFORMATION_CLASS: u32 = 0;
    const STATUS_SUCCESS: i32 = 0;

    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    unsafe fn read_usize(process: HANDLE, address: usize) -> Result<usize> {
        let mut value = 0usize;
        let mut bytes_read = 0usize;
        let ok = ReadProcessMemory(
            process,
            address as *const c_void,
            &mut value as *mut usize as *mut c_void,
            std::mem::size_of::<usize>(),
            &mut bytes_read,
        );
        if ok == 0 || bytes_read != std::mem::size_of::<usize>() {
            anyhow::bail!("ReadProcessMemory failed at 0x{address:x}");
        }
        Ok(value)
    }

    unsafe fn read_env_block(process: HANDLE, address: usize) -> Result<Vec<u16>> {
        const CHUNK_BYTES: usize = 4096;
        const MAX_BYTES: usize = 4 * 1024 * 1024;

        let mut bytes = Vec::new();
        let mut offset = 0usize;
        while offset < MAX_BYTES {
            let mut chunk = [0u8; CHUNK_BYTES];
            let mut bytes_read = 0usize;
            let ok = ReadProcessMemory(
                process,
                (address + offset) as *const c_void,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len(),
                &mut bytes_read,
            );
            if ok == 0 || bytes_read == 0 {
                break;
            }

            bytes.extend_from_slice(&chunk[..bytes_read]);
            if bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| pair == [0, 0])
            {
                break;
            }

            offset += bytes_read;
        }

        let words = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        Ok(words)
    }

    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
    if handle.is_null() {
        anyhow::bail!("OpenProcess failed for pid {pid}");
    }
    let handle = Handle(handle);

    let mut pbi = ProcessBasicInformation {
        reserved1: std::ptr::null_mut(),
        peb_base_address: std::ptr::null_mut(),
        reserved2: [std::ptr::null_mut(); 2],
        unique_process_id: 0,
        reserved3: std::ptr::null_mut(),
    };
    let status = unsafe {
        NtQueryInformationProcess(
            handle.0,
            PROCESS_BASIC_INFORMATION_CLASS,
            &mut pbi as *mut ProcessBasicInformation as *mut c_void,
            std::mem::size_of::<ProcessBasicInformation>() as u32,
            std::ptr::null_mut(),
        )
    };
    if status != STATUS_SUCCESS {
        anyhow::bail!("NtQueryInformationProcess failed with status 0x{status:x}");
    }

    #[cfg(target_pointer_width = "64")]
    const PEB_PROCESS_PARAMETERS_OFFSET: usize = 0x20;
    #[cfg(target_pointer_width = "32")]
    const PEB_PROCESS_PARAMETERS_OFFSET: usize = 0x10;
    #[cfg(target_pointer_width = "64")]
    const RTL_ENVIRONMENT_OFFSET: usize = 0x80;
    #[cfg(target_pointer_width = "32")]
    const RTL_ENVIRONMENT_OFFSET: usize = 0x48;

    let peb = pbi.peb_base_address as usize;
    let process_parameters = unsafe { read_usize(handle.0, peb + PEB_PROCESS_PARAMETERS_OFFSET) }
        .context("reading PEB process parameters")?;
    if process_parameters == 0 {
        return Ok(Vec::new());
    }

    let environment = unsafe { read_usize(handle.0, process_parameters + RTL_ENVIRONMENT_OFFSET) }
        .context("reading process environment pointer")?;
    if environment == 0 {
        return Ok(Vec::new());
    }

    let words = unsafe { read_env_block(handle.0, environment) }
        .context("reading process environment block")?;
    Ok(parse_windows_environment_block(&words))
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(in crate::discovery) fn parse_windows_environment_block(words: &[u16]) -> Vec<String> {
    let mut entries = Vec::new();
    let mut start = 0usize;
    for (idx, word) in words.iter().enumerate() {
        if *word != 0 {
            continue;
        }
        if idx == start {
            break;
        }
        entries.push(String::from_utf16_lossy(&words[start..idx]));
        start = idx + 1;
    }
    entries
}

// Pure string parsing — not gated to macOS so it can be unit-tested from any
// host, even though it is only ever called from the macOS-specific `ps -E`
// fallback path.
#[allow(dead_code)]
pub(in crate::discovery) fn parse_macos_ps_env_candidates(
    stdout: &str,
    var_name: &str,
) -> Vec<(u32, String)> {
    stdout
        .lines()
        .filter_map(|line| parse_macos_ps_env_candidate(line, var_name))
        .collect()
}

#[allow(dead_code)]
pub(in crate::discovery) fn parse_macos_ps_env_candidate(
    line: &str,
    var_name: &str,
) -> Option<(u32, String)> {
    let trimmed = line.trim_start();
    let (pid, rest) = trimmed.split_once(char::is_whitespace)?;
    let pid = pid.parse::<u32>().ok()?;
    let prefix = format!("{var_name}=");
    let value = rest
        .split_whitespace()
        .find_map(|token| token.strip_prefix(&prefix))?;
    if value.is_empty() {
        return None;
    }
    Some((pid, value.to_string()))
}
