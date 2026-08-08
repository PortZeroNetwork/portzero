//! System-wide TCP listener enumeration, for legacy-port monitoring.
//!
//! Split out of `process.rs` (file-size budget): the legacy-port monitor's
//! full-system sweep lives here; per-process port/env primitives stay in
//! [`super::process`].

use super::*;

#[cfg(not(target_os = "macos"))]
use super::process::discover_process_ports;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::process::parse_lsof_line;
use super::process::scan_process_env;

/// A TCP listener observed system-wide, attributed to its owning process.
///
/// Used by the legacy-port monitor to find processes that serve a port directly
/// (bypassing port-zero). Carries enough context (pid, cwd, whether
/// `PZ_TUNNEL` is set) for the monitor's *pure* comparison logic to decide
/// whether the listener is "legacy".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemListener {
    /// The TCP port being listened on.
    pub port: u16,
    /// Owning process id.
    pub pid: u32,
    /// Working directory of the owning process, if discoverable.
    pub cwd: Option<PathBuf>,
    /// Whether the owning process has `PZ_TUNNEL` set (i.e. it is already
    /// managed by us and must NOT be flagged as legacy).
    pub has_port_zero: bool,
}

/// Enumerate every process's listening TCP ports system-wide, attributed to the
/// owning process (pid, cwd, whether `PZ_TUNNEL` is set).
///
/// This is the single public entry point the legacy-port monitor uses.
/// Best-effort and cross-platform: on platforms where port discovery is
/// unavailable it simply returns an empty list.
///
/// PERFORMANCE: this runs inside the discovery loop, so the sweep must never
/// degrade to one subprocess per process on the system. macOS uses a SINGLE
/// system-wide `lsof` (per-pid lsof over hundreds of pids took minutes and
/// wedged the loop); Linux walks /proc per pid, which is pure file I/O with
/// no subprocess in the common case (see `discover_ports_linux`).
pub fn enumerate_system_listeners() -> Vec<SystemListener> {
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut out = Vec::new();
    for (pid_u32, listening) in listeners_by_pid(&sys) {
        if pid_u32 <= 1 || listening.is_empty() {
            continue;
        }

        let cwd = sys
            .process(sysinfo::Pid::from_u32(pid_u32))
            .and_then(|p| p.cwd().map(|c| c.to_path_buf()));
        let has_port_zero = scan_process_env(pid_u32, ENV_VAR_NAME)
            .map(|v| !v.is_empty())
            .unwrap_or(false);

        for lp in listening {
            out.push(SystemListener {
                port: lp.port,
                pid: pid_u32,
                cwd: cwd.clone(),
                has_port_zero,
            });
        }
    }

    out
}

/// pid → listening TCP ports for every process, using the cheapest strategy
/// each OS offers for a SYSTEM-WIDE sweep.
#[cfg(target_os = "macos")]
fn listeners_by_pid(_sys: &System) -> Vec<(u32, Vec<ListeningPort>)> {
    // One lsof for the whole system. Per-pid lsof (discover_ports_lsof) is
    // only acceptable for a handful of already-tagged pids, never for a sweep.
    // Bounded: an unbounded .output() can wedge forever if the child's pipe
    // write-end leaks into a long-lived sibling child (see the docker scans).
    let mut command = std::process::Command::new("lsof");
    command.args(["-iTCP", "-sTCP:LISTEN", "-nP"]);
    let output = match crate::tls::trust::run_command_capture_with_timeout(
        command,
        std::time::Duration::from_secs(10),
        "lsof (system listener sweep)",
    ) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!("system listener sweep lsof failed/timed out: {e:#}");
            return Vec::new();
        }
    };
    if !output.status.success() {
        return Vec::new();
    }
    group_lsof_system_stdout(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(target_os = "macos"))]
fn listeners_by_pid(sys: &System) -> Vec<(u32, Vec<ListeningPort>)> {
    sys.processes()
        .keys()
        .map(|pid| {
            let p = pid.as_u32();
            (p, discover_process_ports(p))
        })
        .collect()
}

/// Group the stdout of a system-wide `lsof -iTCP -sTCP:LISTEN -nP` (PID in
/// column 2) into pid → listening ports. Pure, so it is unit-testable.
///
/// Only called in production on macOS (`listeners_by_pid` above); kept
/// compiling on Linux too so its parsing logic gets unit-tested on every CI
/// runner, not just the macOS one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) fn group_lsof_system_stdout(stdout: &str) -> Vec<(u32, Vec<ListeningPort>)> {
    let mut by_pid: std::collections::BTreeMap<u32, Vec<ListeningPort>> =
        std::collections::BTreeMap::new();
    for line in stdout.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let _command = fields.next();
        let Some(pid) = fields.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if let Some(lp) = parse_lsof_line(line) {
            by_pid.entry(pid).or_default().push(lp);
        }
    }
    by_pid.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn group_lsof_system_stdout_groups_by_pid_and_skips_noise() {
        let stdout = "\
COMMAND   PID     USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
python3   845 loumtech    4u  IPv4 0xdeadbeef        0t0  TCP 127.0.0.1:8080 (LISTEN)
python3   845 loumtech    5u  IPv6 0xdeadbeef        0t0  TCP [::1]:8080 (LISTEN)
node     1201 loumtech   23u  IPv4 0xdeadbeef        0t0  TCP *:5173 (LISTEN)
garbage line with no pid column
sshd     not-a-pid root   3u  IPv4 0xdeadbeef        0t0  TCP *:22 (LISTEN)
";
        let grouped = group_lsof_system_stdout(stdout);
        assert_eq!(grouped.len(), 2);
        let (pid_a, ports_a) = &grouped[0];
        assert_eq!(*pid_a, 845);
        assert_eq!(ports_a.len(), 2);
        assert!(ports_a.iter().all(|p| p.port == 8080));
        let (pid_b, ports_b) = &grouped[1];
        assert_eq!(*pid_b, 1201);
        assert_eq!(ports_b, &vec![ListeningPort::v4_any(5173)]);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn group_lsof_system_stdout_empty_and_header_only() {
        assert!(group_lsof_system_stdout("").is_empty());
        assert!(group_lsof_system_stdout(
            "COMMAND     PID     USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\n"
        )
        .is_empty());
    }
}
